use std::path::{Path, PathBuf};

use crate::deps::versions::InstalledVersions;
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
///
/// ffmpeg state and version are derived **live** here: a PATH lookup plus
/// a short `ffmpeg -version` subprocess (~10ms). Caching them in
/// `versions.json` previously let stale state survive across the user
/// installing or removing a system ffmpeg, producing bogus update arrows.
pub fn check_all(dirs: &AppDirs) -> Vec<DependencyStatus> {
    let versions = InstalledVersions::load(&dirs.data_dir);
    let python_exists = dirs.python_bin().exists();

    vec![
        DependencyStatus {
            name: "Python".into(),
            state: if python_exists {
                DepState::Installed
            } else {
                DepState::Missing
            },
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
            state: if dirs.bin_dir().join(super::ytdlp_binary_name()).exists() {
                DepState::Installed
            } else {
                DepState::Missing
            },
            version: versions.ytdlp.clone(),
            updatable: true,
            update_available: false,
            latest_version: None,
        },
        ffmpeg_status(dirs),
    ]
}

/// Resolve ffmpeg state + version live, mirroring `ffmpeg::ensure()` priority:
/// macOS/Linux prefer system; Windows prefers managed (its shared-build DLLs
/// are required by torchcodec).
fn ffmpeg_status(dirs: &AppDirs) -> DependencyStatus {
    let managed = dirs.bin_dir().join(super::ffmpeg_binary_name());
    let system = super::ffmpeg::find_on_path(super::ffmpeg_binary_name());

    let (binary, state) = resolved(managed, system);

    let (version, state) = match (binary, state) {
        (Some(path), Some(s)) => (query_version(&path), s),
        _ => (None, DepState::Missing),
    };

    DependencyStatus {
        name: "ffmpeg".into(),
        updatable: state == DepState::Installed,
        update_available: false,
        latest_version: None,
        version,
        state,
    }
}

#[cfg(target_os = "windows")]
fn resolved(managed: PathBuf, system: Option<PathBuf>) -> (Option<PathBuf>, Option<DepState>) {
    if managed.exists() {
        return (Some(managed), Some(DepState::Installed));
    }
    if let Some(s) = system {
        return (Some(s), Some(DepState::System));
    }
    (None, None)
}

#[cfg(not(target_os = "windows"))]
fn resolved(managed: PathBuf, system: Option<PathBuf>) -> (Option<PathBuf>, Option<DepState>) {
    if let Some(s) = system {
        return (Some(s), Some(DepState::System));
    }
    if managed.exists() {
        return (Some(managed), Some(DepState::Installed));
    }
    (None, None)
}

/// Run `<bin> -version` synchronously and pluck the version token.
/// Output format: "ffmpeg version 7.1.1 Copyright …" → "7.1.1".
fn query_version(bin: &Path) -> Option<String> {
    let out = std::process::Command::new(bin)
        .arg("-version")
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.split_whitespace().nth(2).map(String::from)
}
