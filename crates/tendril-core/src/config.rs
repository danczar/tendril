use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::dirs::AppDirs;
use crate::error::ConfigError;

/// Persisted user settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Output audio format for separated stems.
    pub output_format: OutputFormat,
    /// GPU backend preference.
    pub gpu_backend: GpuBackend,
    /// Demucs model variant.
    pub model_variant: ModelVariant,
    /// Root directory for stem output.
    pub output_dir: PathBuf,
    /// Whether to preserve the full mix (original audio) in the output directory.
    pub preserve_full_mix: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    Wav,
    Flac,
    Mp3,
    Aac,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GpuBackend {
    Auto,
    #[serde(alias = "coreml")]
    Mps,
    Cuda,
    Cpu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelVariant {
    HtdemucsFineTuned,
    Htdemucs,
}

impl OutputFormat {
    /// File extension for this format.
    pub fn extension(&self) -> &'static str {
        match self {
            OutputFormat::Wav => "wav",
            OutputFormat::Flac => "flac",
            OutputFormat::Mp3 => "mp3",
            OutputFormat::Aac => "m4a",
        }
    }
}

impl ModelVariant {
    /// Map to demucs model name for the Python CLI.
    pub fn model_name(&self) -> &'static str {
        match self {
            ModelVariant::HtdemucsFineTuned => "htdemucs_ft",
            ModelVariant::Htdemucs => "htdemucs",
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            output_format: OutputFormat::Flac,
            gpu_backend: GpuBackend::Auto,
            model_variant: ModelVariant::HtdemucsFineTuned,
            output_dir: AppDirs::default_output_dir(),
            preserve_full_mix: false,
        }
    }
}

impl Config {
    const FILENAME: &str = "settings.toml";

    /// Load config from the platform config directory, or return defaults.
    pub fn load(config_dir: &Path) -> Result<Self, ConfigError> {
        let path = config_dir.join(Self::FILENAME);
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path).map_err(|e| ConfigError::Read {
            path: path.clone(),
            source: e,
        })?;
        let config: Config = toml::from_str(&text)?;
        Ok(config)
    }

    /// Persist config to the platform config directory.
    pub fn save(&self, config_dir: &Path) -> Result<(), ConfigError> {
        let path = config_dir.join(Self::FILENAME);
        let text = toml::to_string_pretty(self)?;
        std::fs::write(&path, text).map_err(|e| ConfigError::Write {
            path: path.clone(),
            source: e,
        })?;
        Ok(())
    }
}
