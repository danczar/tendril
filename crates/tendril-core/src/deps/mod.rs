pub mod demucs_bundle;
pub mod ffmpeg;
pub mod github_release;
pub mod status;
pub mod update_check;
pub mod versions;
pub mod ytdlp;

use crate::dirs::AppDirs;
use crate::error::DependencyError;

pub use demucs_bundle::DownloadProgress;
pub use status::{DepState, DependencyStatus};

/// Manages downloading and updating external binaries.
pub struct DependencyManager {
    dirs: AppDirs,
    client: reqwest::Client,
}

impl DependencyManager {
    pub fn new(dirs: &AppDirs) -> Self {
        Self {
            dirs: dirs.clone(),
            client: reqwest::Client::new(),
        }
    }

    /// Ensure lightweight deps (ffmpeg + yt-dlp) are present.
    /// These are small enough to download silently at startup.
    pub async fn ensure_lightweight(&self) -> Result<(), DependencyError> {
        self.ensure_ffmpeg().await?;
        self.ensure_ytdlp().await?;
        Ok(())
    }

    /// Ensure ALL deps including the heavy demucs bundle.
    /// Call this from the UI download button.
    pub async fn ensure_all(
        &self,
        progress_tx: Option<tokio::sync::watch::Sender<DownloadProgress>>,
    ) -> Result<(), DependencyError> {
        // Heavy dep first (Python + torch + demucs)
        demucs_bundle::ensure(&self.client, &self.dirs, progress_tx).await?;

        // Lightweight deps
        self.ensure_ffmpeg().await?;
        self.ensure_ytdlp().await?;

        // Record yt-dlp and ffmpeg versions
        let mut versions = versions::InstalledVersions::load(&self.dirs.data_dir);
        if versions.ytdlp.is_none() {
            versions.ytdlp = query_ytdlp_version(&self.dirs).await;
        }
        if versions.ffmpeg.is_none() {
            let (ver, source) = query_ffmpeg_version(&self.dirs).await;
            versions.ffmpeg = ver;
            versions.ffmpeg_source = source;
        }
        let _ = versions.save(&self.dirs.data_dir);

        Ok(())
    }

    pub async fn ensure_ffmpeg(&self) -> Result<std::path::PathBuf, DependencyError> {
        ffmpeg::ensure(&self.client, &self.dirs.bin_dir()).await
    }

    pub async fn ensure_ytdlp(&self) -> Result<std::path::PathBuf, DependencyError> {
        ytdlp::ensure(&self.client, &self.dirs.bin_dir()).await
    }

    /// Check if the demucs Python environment is installed.
    pub fn is_demucs_ready(&self) -> bool {
        self.dirs.python_bin().exists()
    }

    /// Get installation status of all dependencies (fast, local-only).
    pub fn check_status(&self) -> Vec<DependencyStatus> {
        status::check_all(&self.dirs)
    }

    /// Check for available updates (hits network).
    pub async fn check_updates(&self) -> Vec<DependencyStatus> {
        let mut statuses = self.check_status();
        let versions = versions::InstalledVersions::load(&self.dirs.data_dir);

        // Check demucs
        if let Some(latest) = update_check::check_demucs_latest(&self.client).await {
            if let Some(dep) = statuses.iter_mut().find(|d| d.name == "demucs") {
                dep.latest_version = Some(latest.clone());
                if dep.version.as_deref() != Some(&latest) && dep.state != DepState::Missing {
                    dep.update_available = true;
                }
            }
        }

        // Check yt-dlp
        if let Some(latest) = update_check::check_ytdlp_latest(&self.client).await {
            if let Some(dep) = statuses.iter_mut().find(|d| d.name == "yt-dlp") {
                dep.latest_version = Some(latest.clone());
                if dep.version.as_deref() != Some(&latest) && dep.state != DepState::Missing {
                    dep.update_available = true;
                }
            }
        }

        // Check ffmpeg (skip if system-provided)
        if versions.ffmpeg_source != versions::FfmpegSource::System {
            if let Some(latest) = update_check::check_ffmpeg_latest(&self.client).await {
                if let Some(dep) = statuses.iter_mut().find(|d| d.name == "ffmpeg") {
                    dep.latest_version = Some(latest.clone());
                    if dep.version.as_deref() != Some(&latest) && dep.state != DepState::Missing {
                        dep.update_available = true;
                    }
                }
            }
        }

        statuses
    }

    /// Update demucs to latest via pip.
    pub async fn update_demucs(&self) -> Result<(), DependencyError> {
        demucs_bundle::update_demucs(&self.dirs).await
    }

    /// Re-download latest yt-dlp.
    pub async fn update_ytdlp(&self) -> Result<(), DependencyError> {
        // Delete existing so ensure() re-downloads
        let path = self.dirs.bin_dir().join(ytdlp_binary_name());
        let _ = std::fs::remove_file(&path);
        self.ensure_ytdlp().await?;

        let mut versions = versions::InstalledVersions::load(&self.dirs.data_dir);
        versions.ytdlp = query_ytdlp_version(&self.dirs).await;
        let _ = versions.save(&self.dirs.data_dir);
        Ok(())
    }

    /// Re-download latest ffmpeg (only if managed, not system).
    pub async fn update_ffmpeg(&self) -> Result<(), DependencyError> {
        let versions = versions::InstalledVersions::load(&self.dirs.data_dir);
        if versions.ffmpeg_source == versions::FfmpegSource::System {
            return Ok(());
        }
        let path = self.dirs.bin_dir().join(ffmpeg_binary_name());
        let _ = std::fs::remove_file(&path);
        self.ensure_ffmpeg().await?;

        let mut versions = versions::InstalledVersions::load(&self.dirs.data_dir);
        let (ver, source) = query_ffmpeg_version(&self.dirs).await;
        versions.ffmpeg = ver;
        versions.ffmpeg_source = source;
        let _ = versions.save(&self.dirs.data_dir);
        Ok(())
    }
}

async fn query_ytdlp_version(dirs: &AppDirs) -> Option<String> {
    let bin = dirs.bin_dir().join(ytdlp_binary_name());
    if !bin.exists() {
        return None;
    }
    let output = tokio::process::Command::new(&bin)
        .arg("--version")
        .output()
        .await
        .ok()?;
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

async fn query_ffmpeg_version(
    dirs: &AppDirs,
) -> (Option<String>, versions::FfmpegSource) {
    let managed = dirs.bin_dir().join(ffmpeg_binary_name());
    let (bin, source) = if managed.exists() {
        (managed, versions::FfmpegSource::Managed)
    } else if let Some(system) = ffmpeg::find_on_path(ffmpeg_binary_name()) {
        (system, versions::FfmpegSource::System)
    } else {
        return (None, versions::FfmpegSource::Managed);
    };

    let output = tokio::process::Command::new(&bin)
        .arg("-version")
        .output()
        .await
        .ok();

    let version = output.and_then(|o| {
        let text = String::from_utf8_lossy(&o.stdout);
        // "ffmpeg version 7.1 ..." → "7.1"
        text.split_whitespace().nth(2).map(String::from)
    });

    (version, source)
}

#[cfg(target_os = "windows")]
fn ytdlp_binary_name() -> &'static str { "yt-dlp.exe" }
#[cfg(not(target_os = "windows"))]
fn ytdlp_binary_name() -> &'static str { "yt-dlp" }

#[cfg(target_os = "windows")]
fn ffmpeg_binary_name() -> &'static str { "ffmpeg.exe" }
#[cfg(not(target_os = "windows"))]
fn ffmpeg_binary_name() -> &'static str { "ffmpeg" }
