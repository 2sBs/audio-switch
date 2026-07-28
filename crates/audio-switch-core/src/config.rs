use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::state::ActiveMic;

pub const CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EndpointIds {
    pub primary_render: Option<String>,
    pub secondary_render: Option<String>,
    pub primary_capture: Option<String>,
    pub secondary_capture: Option<String>,
    pub virtual_render: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceLabels {
    pub primary: String,
    pub secondary: String,
}

impl Default for DeviceLabels {
    fn default() -> Self {
        Self {
            primary: "Primary".into(),
            secondary: "Secondary".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HotkeyConfig {
    pub modifiers: Vec<String>,
    pub key: String,
    pub enabled: bool,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            modifiers: vec!["control".into(), "shift".into()],
            key: "F11".into(),
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioConfig {
    pub sample_rate: u32,
    pub mirror_buffer_ms: u32,
    pub mic_buffer_ms: u32,
    pub cue_enabled: bool,
    pub cue_level_db: f32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            mirror_buffer_ms: 30,
            mic_buffer_ms: 25,
            cue_enabled: true,
            cue_level_db: -18.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppConfig {
    pub version: u32,
    pub endpoints: EndpointIds,
    #[serde(default)]
    pub labels: DeviceLabels,
    pub active_mic: ActiveMic,
    pub hotkey: HotkeyConfig,
    pub audio: AudioConfig,
    pub auto_start: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            endpoints: EndpointIds::default(),
            labels: DeviceLabels::default(),
            active_mic: ActiveMic::Primary,
            hotkey: HotkeyConfig::default(),
            audio: AudioConfig::default(),
            auto_start: true,
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("configuration I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("configuration JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported configuration version {found}; expected {expected}")]
    UnsupportedVersion { found: u32, expected: u32 },
    #[error("LOCALAPPDATA is unavailable")]
    MissingLocalAppData,
}

#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
}

impl ConfigStore {
    pub fn for_current_user() -> Result<Self, ConfigError> {
        let root = std::env::var_os("LOCALAPPDATA").ok_or(ConfigError::MissingLocalAppData)?;
        Ok(Self::at(
            PathBuf::from(root)
                .join("AudioSwitch")
                .join("settings.json"),
        ))
    }

    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load_or_default(&self) -> Result<AppConfig, ConfigError> {
        if !self.path.exists() {
            return Ok(AppConfig::default());
        }

        let config: AppConfig = serde_json::from_slice(&fs::read(&self.path)?)?;
        if config.version != CONFIG_VERSION {
            return Err(ConfigError::UnsupportedVersion {
                found: config.version,
                expected: CONFIG_VERSION,
            });
        }
        Ok(config)
    }

    pub fn save(&self, config: &AppConfig) -> Result<(), ConfigError> {
        if config.version != CONFIG_VERSION {
            return Err(ConfigError::UnsupportedVersion {
                found: config.version,
                expected: CONFIG_VERSION,
            });
        }

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }

        let temp_path = self.path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(config)?;
        {
            let mut file = fs::File::create(&temp_path)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }

        // On Windows rename cannot replace an existing destination. Keeping the old
        // file until the fully flushed replacement exists still prevents partial JSON.
        if self.path.exists() {
            fs::remove_file(&self.path)?;
        }
        fs::rename(temp_path, &self.path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_product_decisions() {
        let config = AppConfig::default();
        assert_eq!(config.active_mic, ActiveMic::Primary);
        assert_eq!(config.labels.primary, "Primary");
        assert_eq!(config.labels.secondary, "Secondary");
        assert_eq!(config.hotkey.modifiers, ["control", "shift"]);
        assert_eq!(config.hotkey.key, "F11");
        assert!(config.auto_start);
        assert!(config.audio.cue_enabled);
    }

    #[test]
    fn round_trips_through_disk() {
        let directory = tempfile::tempdir().unwrap();
        let store = ConfigStore::at(directory.path().join("settings.json"));
        let mut expected = AppConfig {
            active_mic: ActiveMic::Secondary,
            ..AppConfig::default()
        };
        expected.endpoints.primary_render = Some("primary-render-id".into());

        store.save(&expected).unwrap();
        assert_eq!(store.load_or_default().unwrap(), expected);
    }
}
