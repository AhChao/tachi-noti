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

#[cfg(test)]
mod tests {
    use super::*;

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
