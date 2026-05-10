use std::path::{Path, PathBuf};

use crate::deps::github_release;
use crate::error::DependencyError;

#[cfg(target_os = "windows")]
const BINARY_NAME: &str = "ffmpeg.exe";
#[cfg(not(target_os = "windows"))]
const BINARY_NAME: &str = "ffmpeg";

#[cfg(target_os = "windows")]
const FFPROBE_BINARY_NAME: &str = "ffprobe.exe";
#[cfg(not(target_os = "windows"))]
const FFPROBE_BINARY_NAME: &str = "ffprobe";

/// Ensure ffmpeg and ffprobe are available, returning the path to use.
///
/// Priority:
/// - **macOS/Linux**: system PATH > managed > download. Prefer whatever the
///   user already has installed; only fall back to a managed copy if no
///   system ffmpeg is on PATH.
/// - **Windows**: managed > system PATH > download. The Windows managed
///   build ships shared-library DLLs that torchcodec needs at runtime; if
///   they're already there, prefer them over any system ffmpeg.
///
/// Managed downloads come from BtbN/FFmpeg-Builds (Windows shared build) or
/// eugeneware/ffmpeg-static (macOS/Linux static).
pub async fn ensure(client: &reqwest::Client, bin_dir: &Path) -> Result<PathBuf, DependencyError> {
    let managed_path = bin_dir.join(BINARY_NAME);
    let managed_ok = is_install_complete(bin_dir);
    let system = find_on_path(BINARY_NAME);

    #[cfg(target_os = "windows")]
    {
        if managed_ok {
            return Ok(managed_path);
        }
        if let Some(resolved) = system {
            tracing::info!("Using system ffmpeg: {}", resolved.display());
            return Ok(resolved);
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        if let Some(resolved) = system {
            tracing::info!("Using system ffmpeg: {}", resolved.display());
            return Ok(resolved);
        }
        if managed_ok {
            return Ok(managed_path.clone());
        }
    }

    tokio::fs::create_dir_all(bin_dir)
        .await
        .map_err(DependencyError::Extract)?;
    tracing::info!("Downloading ffmpeg...");
    download(client, bin_dir).await?;
    Ok(managed_path)
}

/// Check if the managed ffmpeg installation is complete.
fn is_install_complete(bin_dir: &Path) -> bool {
    let has_ffmpeg = bin_dir.join(BINARY_NAME).exists();
    let has_ffprobe = bin_dir.join(FFPROBE_BINARY_NAME).exists();

    if !has_ffmpeg || !has_ffprobe {
        return false;
    }

    // On Windows, also require shared libraries (DLLs) for torchcodec
    #[cfg(target_os = "windows")]
    if !has_shared_libs(bin_dir) {
        return false;
    }

    true
}

/// Download ffmpeg and ffprobe using the platform-appropriate strategy.
async fn download(client: &reqwest::Client, bin_dir: &Path) -> Result<(), DependencyError> {
    download_into(client, bin_dir).await
}

/// Public alias used by the atomic update flow to download into a
/// staging directory. Same behavior as the internal `download`.
pub async fn download_into(
    client: &reqwest::Client,
    target_dir: &Path,
) -> Result<(), DependencyError> {
    #[cfg(target_os = "windows")]
    {
        return download_shared_build(client, target_dir).await;
    }

    #[cfg(not(target_os = "windows"))]
    {
        download_static_builds(client, target_dir).await
    }
}

// ── Windows: BtbN/FFmpeg-Builds shared build ──────────────────────────

/// Check for FFmpeg shared library DLLs in bin_dir.
#[cfg(target_os = "windows")]
fn has_shared_libs(bin_dir: &Path) -> bool {
    std::fs::read_dir(bin_dir)
        .map(|entries| {
            entries.filter_map(|e| e.ok()).any(|e| {
                let name = e.file_name();
                let name = name.to_string_lossy();
                name.starts_with("avutil-") && name.ends_with(".dll")
            })
        })
        .unwrap_or(false)
}

/// Download the shared FFmpeg build from BtbN/FFmpeg-Builds.
///
/// This provides ffmpeg.exe, ffprobe.exe, and all FFmpeg DLLs needed by
/// torchcodec for audio loading/saving in the demucs pipeline.
#[cfg(target_os = "windows")]
async fn download_shared_build(
    client: &reqwest::Client,
    bin_dir: &Path,
) -> Result<(), DependencyError> {
    let release = github_release::latest_release(client, "BtbN", "FFmpeg-Builds").await?;

    let asset = release
        .assets
        .iter()
        .find(|a| a.name.contains("win64-lgpl-shared") && a.name.ends_with(".zip"))
        .ok_or_else(|| DependencyError::NoRelease {
            tool: "ffmpeg".into(),
        })?;

    tracing::info!("Downloading {}", asset.name);

    let bytes = client
        .get(&asset.browser_download_url)
        .send()
        .await?
        .error_for_status()
        .map_err(|e| DependencyError::GitHubApi {
            message: format!("Failed to download FFmpeg: {e}"),
        })?
        .bytes()
        .await?;

    let bin_dir_owned = bin_dir.to_path_buf();
    tokio::task::spawn_blocking(move || extract_zip_bin(&bytes, &bin_dir_owned))
        .await
        .map_err(|e| DependencyError::Extract(std::io::Error::other(e)))??;

    tracing::info!("FFmpeg shared build installed to {}", bin_dir.display());
    Ok(())
}

/// Extract files from the bin/ subdirectory of a BtbN FFmpeg zip archive.
///
/// The zip contains a top-level directory like
/// `ffmpeg-nX.Y.Z-...-win64-lgpl-shared-X.Y/bin/` with ffmpeg.exe,
/// ffprobe.exe, and all shared library DLLs (avcodec-*.dll, etc.).
#[cfg(target_os = "windows")]
fn extract_zip_bin(zip_bytes: &[u8], bin_dir: &Path) -> Result<(), DependencyError> {
    let reader = std::io::Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(reader)
        .map_err(|e| DependencyError::Extract(std::io::Error::other(e)))?;

    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| DependencyError::Extract(std::io::Error::other(e)))?;

        let path = file.name().to_string();

        // Extract only files from the bin/ subdirectory.
        // Zip paths look like: ffmpeg-nX.Y.Z-xxx-win64-lgpl-shared-X.Y/bin/ffmpeg.exe
        if let Some(pos) = path.find("/bin/") {
            let filename = &path[pos + 5..];
            if !filename.is_empty() && !filename.contains('/') {
                let dest = bin_dir.join(filename);
                let mut out = std::fs::File::create(&dest).map_err(DependencyError::Extract)?;
                std::io::copy(&mut file, &mut out).map_err(DependencyError::Extract)?;
                tracing::debug!("Extracted {filename}");
            }
        }
    }

    Ok(())
}

// ── macOS/Linux: eugeneware/ffmpeg-static ─────────────────────────────

/// Platform-specific asset names from eugeneware/ffmpeg-static releases.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const ASSET_NAME: &str = "ffmpeg-darwin-arm64";
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const ASSET_NAME: &str = "ffmpeg-darwin-x64";
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const ASSET_NAME: &str = "ffmpeg-linux-x64";
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const ASSET_NAME: &str = "ffmpeg-linux-arm64";

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const FFPROBE_ASSET_NAME: &str = "ffprobe-darwin-arm64";
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const FFPROBE_ASSET_NAME: &str = "ffprobe-darwin-x64";
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const FFPROBE_ASSET_NAME: &str = "ffprobe-linux-x64";
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
const FFPROBE_ASSET_NAME: &str = "ffprobe-linux-arm64";

/// Download static ffmpeg and ffprobe from eugeneware/ffmpeg-static.
#[cfg(not(target_os = "windows"))]
async fn download_static_builds(
    client: &reqwest::Client,
    bin_dir: &Path,
) -> Result<(), DependencyError> {
    let release = github_release::latest_release(client, "eugeneware", "ffmpeg-static").await?;

    let ffmpeg_path = bin_dir.join(BINARY_NAME);
    download_asset(client, &release, ASSET_NAME, &ffmpeg_path, "ffmpeg").await?;

    let ffprobe_path = bin_dir.join(FFPROBE_BINARY_NAME);
    download_asset(
        client,
        &release,
        FFPROBE_ASSET_NAME,
        &ffprobe_path,
        "ffprobe",
    )
    .await?;

    Ok(())
}

/// Download a single asset from a GitHub release.
#[cfg(not(target_os = "windows"))]
async fn download_asset(
    client: &reqwest::Client,
    release: &github_release::Release,
    asset_name: &str,
    dest: &Path,
    tool_name: &str,
) -> Result<(), DependencyError> {
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == asset_name)
        .ok_or_else(|| DependencyError::NoRelease {
            tool: tool_name.into(),
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

    tokio::fs::write(dest, &bytes)
        .await
        .map_err(DependencyError::Extract)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))
            .await
            .map_err(DependencyError::Extract)?;
    }

    tracing::info!("{tool_name} downloaded to {}", dest.display());
    Ok(())
}

// ── Shared utilities ──────────────────────────────────────────────────

/// Resolve a binary name to its absolute path: PATH first, then well-known
/// install prefixes so we still find ffmpeg when launched from Finder/Dock.
///
/// macOS .app bundles inherit launchd's minimal PATH, not the user's shell
/// PATH — so a Homebrew ffmpeg at `/opt/homebrew/bin/ffmpeg` is invisible
/// to a `find_on_path` that only consults `$PATH`. The fallback list covers
/// the common package-manager prefixes; first hit wins.
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    if let Some(path_var) = std::env::var_os("PATH")
        && let Some(found) = std::env::split_paths(&path_var).find_map(|dir| {
            let candidate = dir.join(name);
            candidate.is_file().then_some(candidate)
        })
    {
        return Some(found);
    }

    for prefix in fallback_prefixes() {
        let candidate = std::path::Path::new(prefix).join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn fallback_prefixes() -> &'static [&'static str] {
    &[
        "/opt/homebrew/bin", // Apple Silicon Homebrew
        "/usr/local/bin",    // Intel Homebrew, manual installs
        "/opt/local/bin",    // MacPorts
        "/usr/bin",          // system (rare for ffmpeg)
    ]
}

#[cfg(target_os = "linux")]
fn fallback_prefixes() -> &'static [&'static str] {
    &["/usr/local/bin", "/usr/bin", "/snap/bin"]
}

#[cfg(target_os = "windows")]
fn fallback_prefixes() -> &'static [&'static str] {
    &[]
}
