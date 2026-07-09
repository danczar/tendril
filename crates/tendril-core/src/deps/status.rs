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
    /// Where the dependency lives on disk (binary path or package dir),
    /// when installed. For ffmpeg this is the *resolved* binary — managed
    /// or system — so the UI shows which one the pipeline actually runs.
    pub location: Option<PathBuf>,
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
    let python_bin = dirs.python_bin();
    let python_exists = python_bin.exists();
    let site_packages = super::demucs_bundle::site_packages_dir(dirs);
    let torch_dir = site_packages.join("torch");
    let demucs_dir = site_packages.join("demucs");
    let ytdlp_bin = dirs.bin_dir().join(super::ytdlp_binary_name());
    let ytdlp_exists = ytdlp_bin.exists();

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
            location: python_exists.then_some(python_bin),
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
            location: torch_dir.is_dir().then_some(torch_dir),
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
            location: demucs_dir.is_dir().then_some(demucs_dir),
        },
        DependencyStatus {
            name: "yt-dlp".into(),
            state: if ytdlp_exists {
                DepState::Installed
            } else {
                DepState::Missing
            },
            version: versions.ytdlp.clone(),
            updatable: true,
            update_available: false,
            latest_version: None,
            location: ytdlp_exists.then_some(ytdlp_bin),
        },
        ffmpeg_status(dirs),
    ]
}

/// Resolve ffmpeg state + version live, mirroring `ffmpeg::ensure()` priority:
/// macOS/Linux prefer a *working* system ffmpeg; Windows prefers managed. A
/// system ffmpeg that fails to execute (e.g. a Homebrew build with missing
/// dylibs) is ignored here just as it is in `ensure`, so the UI never advertises
/// an ffmpeg the pipeline can't actually use.
fn ffmpeg_status(dirs: &AppDirs) -> DependencyStatus {
    let managed = dirs.bin_dir().join(super::ffmpeg_binary_name());
    let system = super::ffmpeg::find_working_system_ffmpeg();

    let (binary, state) = resolved(managed, system);

    let (version, state, location) = match (binary, state) {
        (Some(path), Some(s)) => (query_version(&path), s, Some(path)),
        _ => (None, DepState::Missing, None),
    };

    DependencyStatus {
        name: "ffmpeg".into(),
        updatable: state == DepState::Installed,
        update_available: false,
        latest_version: None,
        version,
        state,
        location,
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
