use std::path::{Path, PathBuf};

use directories::ProjectDirs;

/// Platform-appropriate directories for Tendril data.
#[derive(Clone)]
pub struct AppDirs {
    /// Config files (e.g. settings.toml).
    pub config_dir: PathBuf,
    /// Persistent data: models, downloaded binaries.
    pub data_dir: PathBuf,
    /// Cache: temporary downloads, intermediate audio files.
    pub cache_dir: PathBuf,
}

impl AppDirs {
    /// Resolve platform directories and create them if needed.
    pub fn resolve() -> std::io::Result<Self> {
        let proj = ProjectDirs::from("com", "tendril", "Tendril")
            .expect("home directory must exist");

        let dirs = Self {
            config_dir: proj.config_dir().to_path_buf(),
            data_dir: proj.data_dir().to_path_buf(),
            cache_dir: proj.cache_dir().to_path_buf(),
        };
        dirs.ensure_created()?;
        Ok(dirs)
    }

    fn ensure_created(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.config_dir)?;
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(&self.cache_dir)?;
        Ok(())
    }

    /// Directory where external binaries (ffmpeg, yt-dlp) are stored.
    pub fn bin_dir(&self) -> PathBuf {
        self.data_dir.join("bin")
    }

    /// Directory where demucs models are cached.
    pub fn models_dir(&self) -> PathBuf {
        self.data_dir.join("models")
    }

    /// Directory where the bundled demucs Python environment lives.
    pub fn demucs_dir(&self) -> PathBuf {
        self.data_dir.join("demucs")
    }

    /// Path to the bundled Python binary.
    pub fn python_bin(&self) -> PathBuf {
        let demucs = self.demucs_dir();
        if cfg!(target_os = "windows") {
            demucs.join("python").join("python.exe")
        } else {
            demucs.join("python").join("bin").join("python3")
        }
    }

    /// Default output directory for separated stems.
    pub fn default_output_dir() -> PathBuf {
        directories::UserDirs::new()
            .and_then(|u| u.audio_dir().map(Path::to_path_buf))
            .unwrap_or_else(|| {
                directories::UserDirs::new()
                    .map(|u| u.home_dir().join("Music"))
                    .expect("home directory must exist")
            })
            .join("Tendril")
    }
}
