use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub enabled: bool,
    /// The loudness level (dBFS-ish) we steer everything towards — the user's comfort level.
    pub target_loudness: f32,
    /// How aggressively we correct (0..1.5; >1.0 = extra-aggressive).
    pub strength: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            // Typical comfortable movie dialogue level when the device slider sits at 50%.
            target_loudness: -24.0,
            strength: 0.65,
        }
    }
}

impl Config {
    /// Clamp settings loaded from disk so malformed or stale values cannot
    /// reach the audio controller or produce unusable UI state.
    pub fn normalized(mut self) -> Self {
        let defaults = Self::default();
        if !self.target_loudness.is_finite() {
            self.target_loudness = defaults.target_loudness;
        }
        if !self.strength.is_finite() {
            self.strength = defaults.strength;
        }
        self.target_loudness = self.target_loudness.clamp(-80.0, -12.0);
        self.strength = self.strength.clamp(0.05, 1.5);
        self
    }

    pub fn config_path() -> PathBuf {
        if let Some(dir) = dirs_path() {
            let _ = std::fs::create_dir_all(&dir);
            return dir.join("config.json");
        }
        PathBuf::from("baffle-config.json")
    }

    pub fn load() -> Self {
        let path = Self::config_path();
        match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str::<Self>(&s)
                .map(Self::normalized)
                .unwrap_or_else(|_| Self::default()),
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path();
        let s = serde_json::to_string_pretty(&self.normalized())?;
        std::fs::write(path, s)?;
        Ok(())
    }
}

fn dirs_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("Baffle"))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(|d| PathBuf::from(d).join("Library/Application Support/Baffle"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|d| PathBuf::from(d).join(".config")))
            .map(|d| d.join("baffle"))
    }
}
