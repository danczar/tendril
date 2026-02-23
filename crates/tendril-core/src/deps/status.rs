use crate::deps::versions::{FfmpegSource, InstalledVersions};
use crate::dirs::AppDirs;

/// Installation state of a dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DepState {
    /// Installed and managed by Tendril.
    Installed,
    /// Found on system PATH (ffmpeg only).
    System,
    /// Not installed.
    Missing,
}

/// Status of a single dependency for UI display.
#[derive(Debug, Clone)]
pub struct DependencyStatus {
    pub name: String,
    pub state: DepState,
    pub version: Option<String>,
    /// Whether the dep can be updated (not pinned, not system).
    pub updatable: bool,
    /// Whether a newer version is available (set async by update_check).
    pub update_available: bool,
    pub latest_version: Option<String>,
}

/// Check the installation status of all dependencies.
///
/// This is a fast, local-only check (no network). For update availability,
/// call `update_check` functions separately.
pub fn check_all(dirs: &AppDirs) -> Vec<DependencyStatus> {
    let versions = InstalledVersions::load(&dirs.data_dir);
    let python_exists = dirs.python_bin().exists();

    vec![
        DependencyStatus {
            name: "Python".into(),
            state: if python_exists { DepState::Installed } else { DepState::Missing },
            version: versions.python.clone(),
            updatable: false, // pinned per Tendril release
            update_available: false,
            latest_version: None,
        },
        DependencyStatus {
            name: "PyTorch".into(),
            state: if python_exists && versions.torch.is_some() {
                DepState::Installed
            } else {
                DepState::Missing
            },
            version: versions.torch.clone(),
            updatable: false, // pinned per Tendril release
            update_available: false,
            latest_version: None,
        },
        DependencyStatus {
            name: "demucs".into(),
            state: if versions.demucs.is_some() {
                DepState::Installed
            } else {
                DepState::Missing
            },
            version: versions.demucs.clone(),
            updatable: true,
            update_available: false,
            latest_version: None,
        },
        DependencyStatus {
            name: "yt-dlp".into(),
            state: if dirs.bin_dir().join(ytdlp_binary_name()).exists() {
                DepState::Installed
            } else {
                DepState::Missing
            },
            version: versions.ytdlp.clone(),
            updatable: true,
            update_available: false,
            latest_version: None,
        },
        DependencyStatus {
            name: "ffmpeg".into(),
            state: if versions.ffmpeg_source == FfmpegSource::System {
                DepState::System
            } else if dirs.bin_dir().join(ffmpeg_binary_name()).exists() {
                DepState::Installed
            } else if super::ffmpeg::find_on_path(ffmpeg_binary_name()).is_some() {
                DepState::System
            } else {
                DepState::Missing
            },
            version: versions.ffmpeg.clone(),
            updatable: versions.ffmpeg_source != FfmpegSource::System,
            update_available: false,
            latest_version: None,
        },
    ]
}

#[cfg(target_os = "windows")]
fn ytdlp_binary_name() -> &'static str { "yt-dlp.exe" }
#[cfg(not(target_os = "windows"))]
fn ytdlp_binary_name() -> &'static str { "yt-dlp" }

#[cfg(target_os = "windows")]
fn ffmpeg_binary_name() -> &'static str { "ffmpeg.exe" }
#[cfg(not(target_os = "windows"))]
fn ffmpeg_binary_name() -> &'static str { "ffmpeg" }
