use std::path::Path;

use serde::{Deserialize, Serialize};

const FILENAME: &str = "versions.json";

/// Tracks installed versions of dependencies that Tendril manages.
///
/// ffmpeg is intentionally absent: its state and version are derived
/// live on every read (system PATH lookup + `ffmpeg -version` subprocess),
/// so there is nothing to cache here. Persisting it caused stale-state
/// bugs where a managed-era version string survived after the user
/// installed a system ffmpeg, producing bogus "update available" arrows.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InstalledVersions {
    pub python: Option<String>,
    pub torch: Option<String>,
    pub demucs: Option<String>,
    pub ytdlp: Option<String>,
}

impl InstalledVersions {
    /// Load version info from disk, or return defaults if not found.
    /// Unknown fields (e.g. legacy `ffmpeg`/`ffmpeg_source`) are ignored.
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
