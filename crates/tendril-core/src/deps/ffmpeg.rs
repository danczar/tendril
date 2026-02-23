use std::path::{Path, PathBuf};

use crate::deps::github_release;
use crate::error::DependencyError;

#[cfg(target_os = "windows")]
const BINARY_NAME: &str = "ffmpeg.exe";
#[cfg(not(target_os = "windows"))]
const BINARY_NAME: &str = "ffmpeg";

/// Platform-specific asset name from eugeneware/ffmpeg-static releases.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const ASSET_NAME: &str = "ffmpeg-darwin-arm64";
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const ASSET_NAME: &str = "ffmpeg-darwin-x64";
#[cfg(all(target_os = "windows", target_arch = "x86_64"))]
const ASSET_NAME: &str = "ffmpeg-win32-x64";
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const ASSET_NAME: &str = "ffmpeg-linux-x64";
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const ASSET_NAME: &str = "ffmpeg-linux-arm64";

/// Ensure ffmpeg is available. Checks bin_dir first, then system PATH,
/// then downloads from GitHub.
pub async fn ensure(
    client: &reqwest::Client,
    bin_dir: &Path,
) -> Result<PathBuf, DependencyError> {
    // Check managed bin_dir
    let path = bin_dir.join(BINARY_NAME);
    if path.exists() {
        return Ok(path);
    }

    // Check system PATH — resolve to absolute path so other tools can find it
    if let Some(resolved) = find_on_path(BINARY_NAME) {
        tracing::info!("Using system ffmpeg: {}", resolved.display());
        return Ok(resolved);
    }

    // Download from GitHub
    std::fs::create_dir_all(bin_dir).map_err(DependencyError::Extract)?;

    tracing::info!("Downloading ffmpeg...");
    let release =
        github_release::latest_release(client, "eugeneware", "ffmpeg-static").await?;
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == ASSET_NAME)
        .ok_or_else(|| DependencyError::NoRelease {
            tool: "ffmpeg".into(),
        })?;

    let bytes = client
        .get(&asset.browser_download_url)
        .send()
        .await?
        .error_for_status()
        .map_err(|e| DependencyError::GitHubApi {
            message: e.to_string(),
        })?
        .bytes()
        .await?;

    std::fs::write(&path, &bytes).map_err(DependencyError::Extract)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .map_err(DependencyError::Extract)?;
    }

    tracing::info!("ffmpeg downloaded to {}", path.display());
    Ok(path)
}

/// Resolve a binary name to its absolute path on the system PATH.
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var).find_map(|dir| {
        let candidate = dir.join(name);
        if candidate.is_file() {
            Some(candidate)
        } else {
            None
        }
    })
}
