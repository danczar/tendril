use std::path::{Path, PathBuf};

use crate::deps::github_release;
use crate::error::DependencyError;

#[cfg(target_os = "windows")]
const BINARY_NAME: &str = "yt-dlp.exe";
#[cfg(not(target_os = "windows"))]
const BINARY_NAME: &str = "yt-dlp";

#[cfg(target_os = "macos")]
const ASSET_NAME: &str = "yt-dlp_macos";
#[cfg(target_os = "windows")]
const ASSET_NAME: &str = "yt-dlp.exe";
#[cfg(target_os = "linux")]
const ASSET_NAME: &str = "yt-dlp_linux";

/// Ensure yt-dlp is present in `bin_dir`, downloading if necessary.
pub async fn ensure(
    client: &reqwest::Client,
    bin_dir: &Path,
) -> Result<PathBuf, DependencyError> {
    let path = bin_dir.join(BINARY_NAME);
    if path.exists() {
        return Ok(path);
    }

    std::fs::create_dir_all(bin_dir).map_err(DependencyError::Extract)?;

    tracing::info!("Downloading yt-dlp...");
    let release = github_release::latest_release(client, "yt-dlp", "yt-dlp").await?;
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == ASSET_NAME)
        .ok_or_else(|| DependencyError::NoRelease {
            tool: "yt-dlp".into(),
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

    tracing::info!("yt-dlp downloaded to {}", path.display());
    Ok(path)
}
