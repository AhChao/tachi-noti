use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize, Debug)]
#[serde(default)]
pub struct Config {
    pub sounds: Sounds,
    pub focus_suppression: bool,
    pub min_duration_secs: u64,
    /// None = auto-detect; "terminal-notifier" | "osascript" to force.
    pub backend: Option<String>,
    pub max_body_len: usize,
    /// Image shown on the notification (terminal-notifier only).
    /// None = bundled Tachi portrait; "" = no image; otherwise a file path.
    pub icon: Option<String>,
    /// Menu bar shows a red warning when any usage window reaches this
    /// percentage. 0 disables the warning.
    pub usage_alert_pct: u8,
}

#[derive(Deserialize, Debug)]
#[serde(default)]
pub struct Sounds {
    /// Empty string disables the sound.
    pub stop: String,
    pub attention: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            sounds: Sounds::default(),
            focus_suppression: true,
            min_duration_secs: 0,
            backend: None,
            max_body_len: 120,
            icon: None,
            usage_alert_pct: 80,
        }
    }
}

impl Default for Sounds {
    fn default() -> Self {
        Sounds { stop: "Glass".into(), attention: "Basso".into() }
    }
}

pub fn config_path() -> Option<PathBuf> {
    // Deliberately ~/.config (not dirs::config_dir, which is Application Support on macOS).
    dirs::home_dir().map(|h| h.join(".config/tachi-noti/config.toml"))
}

/// Load config; any failure (missing file, bad TOML) falls back to defaults so the
/// hook path can never fail because of configuration.
pub fn load() -> Config {
    let Some(path) = config_path() else { return Config::default() };
    let Ok(text) = std::fs::read_to_string(&path) else { return Config::default() };
    toml::from_str(&text).unwrap_or_default()
}

/// Persist the completion sound chosen from the tachi-bar menu. Edits the
/// config file in place (toml_edit keeps comments and unknown keys intact).
pub fn set_stop_sound(name: &str) -> Result<(), String> {
    let path = config_path().ok_or("cannot resolve home directory")?;
    set_stop_sound_at(&path, name)
}

fn set_stop_sound_at(path: &std::path::Path, name: &str) -> Result<(), String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut doc = text.parse::<toml_edit::DocumentMut>().map_err(|e| format!("config is not valid TOML: {e}"))?;
    if doc.get("sounds").map(|s| !s.is_table()).unwrap_or(false) {
        return Err("config key 'sounds' exists but is not a table".into());
    }
    doc["sounds"]["stop"] = toml_edit::value(name);
    let dir = path.parent().ok_or("config path has no parent")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(".config.toml.tmp.{}", std::process::id()));
    std::fs::write(&tmp, doc.to_string()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_stop_sound_preserves_file() {
        let dir = std::env::temp_dir().join(format!("tachi-noti-test-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("config.toml");
        std::fs::write(&p, "# my notes\nmin_duration_secs = 15\n\n[sounds]\nattention = \"Ping\"\n").unwrap();
        set_stop_sound_at(&p, "TachiBark").unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(text.contains("# my notes"), "comments survive");
        assert!(text.contains("min_duration_secs = 15"));
        let c: Config = toml::from_str(&text).unwrap();
        assert_eq!(c.sounds.stop, "TachiBark");
        assert_eq!(c.sounds.attention, "Ping");
        // Missing file → created from scratch.
        let p2 = dir.join("fresh.toml");
        set_stop_sound_at(&p2, "Glass").unwrap();
        let c: Config = toml::from_str(&std::fs::read_to_string(&p2).unwrap()).unwrap();
        assert_eq!(c.sounds.stop, "Glass");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn defaults() {
        let c = Config::default();
        assert_eq!(c.sounds.stop, "Glass");
        assert_eq!(c.sounds.attention, "Basso");
        assert!(c.focus_suppression);
        assert_eq!(c.min_duration_secs, 0);
        assert!(c.backend.is_none());
        assert_eq!(c.max_body_len, 120);
    }

    #[test]
    fn partial_toml_fills_defaults() {
        let c: Config = toml::from_str("min_duration_secs = 15\n[sounds]\nstop = \"Ping\"\n").unwrap();
        assert_eq!(c.min_duration_secs, 15);
        assert_eq!(c.sounds.stop, "Ping");
        assert_eq!(c.sounds.attention, "Basso");
        assert!(c.focus_suppression);
    }

    #[test]
    fn bad_toml_falls_back() {
        let c: Result<Config, _> = toml::from_str("not valid {{{");
        assert!(c.is_err()); // load() maps this to Config::default()
    }
}
