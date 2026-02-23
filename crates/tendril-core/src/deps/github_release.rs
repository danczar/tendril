use serde::Deserialize;

use crate::error::DependencyError;

#[derive(Debug, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
}

/// Fetch the latest release from a GitHub repository.
pub async fn latest_release(
    client: &reqwest::Client,
    owner: &str,
    repo: &str,
) -> Result<Release, DependencyError> {
    let url = format!(
        "https://api.github.com/repos/{owner}/{repo}/releases/latest"
    );
    let resp = client
        .get(&url)
        .header("User-Agent", "tendril")
        .send()
        .await?
        .error_for_status()
        .map_err(|e| DependencyError::GitHubApi {
            message: e.to_string(),
        })?;
    let release: Release = resp.json().await?;
    Ok(release)
}

/// Fetch a specific release by tag from a GitHub repository.
pub async fn tagged_release(
    client: &reqwest::Client,
    owner: &str,
    repo: &str,
    tag: &str,
) -> Result<Release, DependencyError> {
    let url = format!(
        "https://api.github.com/repos/{owner}/{repo}/releases/tags/{tag}"
    );
    let resp = client
        .get(&url)
        .header("User-Agent", "tendril")
        .send()
        .await?
        .error_for_status()
        .map_err(|e| DependencyError::GitHubApi {
            message: e.to_string(),
        })?;
    let release: Release = resp.json().await?;
    Ok(release)
}
