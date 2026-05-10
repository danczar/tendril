use std::path::Path;

use serde::{Deserialize, Serialize};

const FILENAME: &str = "versions.json";

/// Source of the ffmpeg binary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FfmpegSource {
    #[default]
    Managed,
    System,
}

/// Tracks installed versions of external dependencies.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InstalledVersions {
    pub python: Option<String>,
    pub torch: Option<String>,
    pub demucs: Option<String>,
    pub ytdlp: Option<String>,
    pub ffmpeg: Option<String>,
    #[serde(default)]
    pub ffmpeg_source: FfmpegSource,
}

impl InstalledVersions {
    /// Load version info from disk, or return defaults if not found.
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(FILENAME);
        if !path.exists() {
            return Self::default();
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Persist version info to disk.
    pub fn save(&self, data_dir: &Path) -> std::io::Result<()> {
        let path = data_dir.join(FILENAME);
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&path, text)
    }
}
