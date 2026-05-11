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
    /// Whether to render an instrumental mix (drums + bass + other).
    pub create_instrumental: bool,
    /// Target integrated loudness in LUFS applied to every output.
    /// -14 LUFS matches the modern streaming reference (Spotify, YouTube,
    /// Apple Music auto-leveling target).
    pub target_lufs: f32,
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
    pub const fn extension(&self) -> &'static str {
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
    pub const fn model_name(&self) -> &'static str {
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
            create_instrumental: true,
            target_lufs: -14.0,
        }
    }
}

impl Config {
    const FILENAME: &str = "settings.toml";
    const BAD_FILENAME: &str = "settings.toml.bad";
    const TMP_FILENAME: &str = "settings.toml.tmp";

    /// Load config from the platform config directory, or return defaults.
    ///
    /// If the file is missing, defaults are returned. If the file exists but
    /// fails to parse (corrupt/truncated), it is moved aside as
    /// `settings.toml.bad` and defaults are returned, so the app does not
    /// brick at startup.
    pub fn load(config_dir: &Path) -> Result<Self, ConfigError> {
        let path = config_dir.join(Self::FILENAME);
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path).map_err(|e| ConfigError::Read {
            path: path.clone(),
            source: e,
        })?;
        match toml::from_str::<Config>(&text) {
            Ok(config) => Ok(config),
            Err(e) => {
                let bad_path = config_dir.join(Self::BAD_FILENAME);
                tracing::warn!(
                    "Failed to parse {}: {}. Moving aside to {} and using defaults.",
                    path.display(),
                    e,
                    bad_path.display()
                );
                if let Err(rename_err) = std::fs::rename(&path, &bad_path) {
                    tracing::warn!(
                        "Could not move corrupt config aside ({}): {}",
                        bad_path.display(),
                        rename_err
                    );
                }
                Ok(Self::default())
            }
        }
    }

    /// Persist config to the platform config directory atomically.
    ///
    /// Writes to `settings.toml.tmp` in the same directory, then renames
    /// over `settings.toml`. Same-directory rename is atomic on POSIX and
    /// reliable on Windows when the target isn't locked. The `.tmp` file
    /// is removed on serialization or write failure to avoid leaving stale
    /// junk behind.
    pub fn save(&self, config_dir: &Path) -> Result<(), ConfigError> {
        let path = config_dir.join(Self::FILENAME);
        let tmp_path = config_dir.join(Self::TMP_FILENAME);
        let text = match toml::to_string_pretty(self) {
            Ok(t) => t,
            Err(e) => {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(ConfigError::Serialize(e));
            }
        };
        if let Err(e) = std::fs::write(&tmp_path, &text) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(ConfigError::Write {
                path: tmp_path,
                source: e,
            });
        }
        if let Err(e) = std::fs::rename(&tmp_path, &path) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(ConfigError::Write { path, source: e });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Build a unique temp directory under `std::env::temp_dir()` without
    /// pulling in the `tempfile` crate. Caller is responsible for cleanup.
    fn fresh_test_dir(label: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("tendril-config-test-{label}-{pid}-{n}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create test dir");
        dir
    }

    #[test]
    fn save_is_atomic_and_round_trips() {
        let dir = fresh_test_dir("save");
        let cfg = Config::default();
        cfg.save(&dir).expect("save");
        // .tmp file should not linger after a successful save.
        assert!(!dir.join(Config::TMP_FILENAME).exists());
        assert!(dir.join(Config::FILENAME).exists());
        let loaded = Config::load(&dir).expect("load");
        assert_eq!(loaded.output_format, cfg.output_format);
        assert_eq!(loaded.gpu_backend, cfg.gpu_backend);
        assert_eq!(loaded.model_variant, cfg.model_variant);
        assert_eq!(loaded.preserve_full_mix, cfg.preserve_full_mix);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_recovers_from_corrupt_file() {
        let dir = fresh_test_dir("corrupt");
        let path = dir.join(Config::FILENAME);
        std::fs::write(&path, "this is = not valid = toml === !!!").expect("write garbage");
        let cfg = Config::load(&dir).expect("load should not error on corrupt file");
        // Defaults returned.
        assert_eq!(cfg.output_format, Config::default().output_format);
        // Corrupt file moved aside.
        assert!(dir.join(Config::BAD_FILENAME).exists());
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
