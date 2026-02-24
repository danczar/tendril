use std::path::PathBuf;

use tokio::sync::watch;

use crate::deps::versions::InstalledVersions;
use crate::dirs::AppDirs;
use crate::error::DependencyError;

/// Progress update during dependency downloads.
#[derive(Debug, Clone)]
pub struct DownloadProgress {
    pub tool: String,
    pub fraction: f32,
    pub message: String,
}

impl Default for DownloadProgress {
    fn default() -> Self {
        Self {
            tool: String::new(),
            fraction: 0.0,
            message: String::new(),
        }
    }
}

/// Python version to install from python-build-standalone.
const PYTHON_VERSION: &str = "3.13.12";
const PYTHON_BUILD_TAG: &str = "20260203";

/// Platform triple for python-build-standalone.
fn python_triple() -> &'static str {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    { "aarch64-apple-darwin" }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    { "x86_64-apple-darwin" }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    { "x86_64-pc-windows-msvc" }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    { "x86_64-unknown-linux-gnu" }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    { "aarch64-unknown-linux-gnu" }
}

/// Build the download URL for python-build-standalone.
fn python_download_url() -> String {
    let triple = python_triple();
    let tarball = format!(
        "cpython-{PYTHON_VERSION}+{PYTHON_BUILD_TAG}-{triple}-install_only_stripped.tar.gz"
    );
    format!(
        "https://github.com/astral-sh/python-build-standalone/releases/download/{PYTHON_BUILD_TAG}/{tarball}"
    )
}

/// Download standalone Python, then pip install torch + demucs.
///
/// Steps:
/// 1. Download python-build-standalone from Astral's GitHub releases
/// 2. Extract to `data_dir/demucs/python/`
/// 3. `pip install torch demucs` into the managed Python
/// 4. Record installed versions
pub async fn ensure(
    client: &reqwest::Client,
    dirs: &AppDirs,
    progress_tx: Option<watch::Sender<DownloadProgress>>,
) -> Result<PathBuf, DependencyError> {
    let python_bin = dirs.python_bin();
    if python_bin.exists() {
        // Python exists — make sure demucs is installed too
        let versions = InstalledVersions::load(&dirs.data_dir);
        if versions.demucs.is_some() {
            return Ok(python_bin);
        }
        // Python present but demucs missing — just pip install
        return install_demucs(dirs, &python_bin, &progress_tx).await;
    }

    let send = |tool: &str, fraction: f32, msg: &str| {
        if let Some(tx) = &progress_tx {
            let _ = tx.send(DownloadProgress {
                tool: tool.into(),
                fraction,
                message: msg.into(),
            });
        }
    };

    let demucs_dir = dirs.demucs_dir();
    std::fs::create_dir_all(&demucs_dir).map_err(DependencyError::Extract)?;

    // ── Step 1: Download python-build-standalone ──
    let url = python_download_url();
    send("Python", 0.0, "Downloading Python...");
    tracing::info!("Downloading Python from {url}");

    let resp = client
        .get(&url)
        .send()
        .await?
        .error_for_status()
        .map_err(|e| DependencyError::GitHubApi {
            message: format!("Failed to download Python: {e}"),
        })?;

    let total_size = resp.content_length().unwrap_or(0);
    let mut downloaded: u64 = 0;
    let mut body_bytes = Vec::with_capacity(total_size as usize);

    use futures::StreamExt;
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        downloaded += chunk.len() as u64;
        body_bytes.extend_from_slice(&chunk);

        if total_size > 0 {
            let frac = downloaded as f32 / total_size as f32;
            let mb_done = downloaded / (1024 * 1024);
            let mb_total = total_size / (1024 * 1024);
            send(
                "Python",
                frac * 0.3, // 0–30% for Python download
                &format!("Downloading Python ({mb_done}/{mb_total} MB)..."),
            );
        }
    }

    // ── Step 2: Extract ──
    send("Python", 0.3, "Extracting Python...");
    tracing::info!("Extracting Python to {}", demucs_dir.display());

    let demucs_dir_clone = demucs_dir.clone();
    tokio::task::spawn_blocking(move || {
        let decoder = flate2::read::GzDecoder::new(&body_bytes[..]);
        let mut archive = tar::Archive::new(decoder);
        archive.unpack(&demucs_dir_clone)
    })
    .await
    .map_err(|e| DependencyError::Extract(std::io::Error::new(std::io::ErrorKind::Other, e)))?
    .map_err(DependencyError::Extract)?;

    // Set executable permissions
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if python_bin.exists() {
            std::fs::set_permissions(&python_bin, std::fs::Permissions::from_mode(0o755))
                .map_err(DependencyError::Extract)?;
        }
    }

    if !python_bin.exists() {
        return Err(DependencyError::BinaryNotFound {
            path: python_bin.clone(),
        });
    }

    // ── Step 3: pip install torch + demucs ──
    send("PyTorch", 0.35, "Installing PyTorch (this may take a few minutes)...");

    install_torch(&python_bin).await?;

    send("demucs", 0.75, "Installing demucs...");
    tracing::info!("Installing demucs...");

    let pip_demucs = tokio::process::Command::new(&python_bin)
        .args(["-m", "pip", "install", "--no-input", "demucs", "soundfile"])
        .output()
        .await
        .map_err(DependencyError::Extract)?;

    if !pip_demucs.status.success() {
        let stderr = String::from_utf8_lossy(&pip_demucs.stderr);
        return Err(DependencyError::GitHubApi {
            message: format!("pip install demucs failed: {stderr}"),
        });
    }

    // ── Step 4: Record versions ──
    send("demucs", 0.95, "Recording versions...");

    let mut versions = InstalledVersions::load(&dirs.data_dir);
    versions.python = query_version(&python_bin, &["--version"]).await;
    versions.torch = query_python_version(&python_bin, "torch").await;
    versions.demucs = query_python_version(&python_bin, "demucs").await;
    let _ = versions.save(&dirs.data_dir);

    send("demucs", 1.0, "Python environment ready!");
    tracing::info!("Python environment ready at {}", python_bin.display());

    Ok(python_bin)
}

/// Install just demucs into an existing Python (for recovery/update).
async fn install_demucs(
    dirs: &AppDirs,
    python_bin: &std::path::Path,
    progress_tx: &Option<watch::Sender<DownloadProgress>>,
) -> Result<PathBuf, DependencyError> {
    if let Some(tx) = progress_tx {
        let _ = tx.send(DownloadProgress {
            tool: "demucs".into(),
            fraction: 0.5,
            message: "Installing demucs...".into(),
        });
    }

    let output = tokio::process::Command::new(python_bin)
        .args(["-m", "pip", "install", "--no-input", "demucs", "soundfile"])
        .output()
        .await
        .map_err(DependencyError::Extract)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DependencyError::GitHubApi {
            message: format!("pip install demucs failed: {stderr}"),
        });
    }

    let mut versions = InstalledVersions::load(&dirs.data_dir);
    versions.demucs = query_python_version(python_bin, "demucs").await;
    let _ = versions.save(&dirs.data_dir);

    if let Some(tx) = progress_tx {
        let _ = tx.send(DownloadProgress {
            tool: "demucs".into(),
            fraction: 1.0,
            message: "demucs installed!".into(),
        });
    }

    Ok(python_bin.to_path_buf())
}

/// Update demucs to the latest version via pip.
pub async fn update_demucs(dirs: &AppDirs) -> Result<(), DependencyError> {
    let python_bin = dirs.python_bin();
    if !python_bin.exists() {
        return Err(DependencyError::BinaryNotFound { path: python_bin });
    }

    let output = tokio::process::Command::new(&python_bin)
        .args(["-m", "pip", "install", "--no-input", "--upgrade", "demucs"])
        .output()
        .await
        .map_err(DependencyError::Extract)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DependencyError::GitHubApi {
            message: format!("pip upgrade demucs failed: {stderr}"),
        });
    }

    let mut versions = InstalledVersions::load(&dirs.data_dir);
    versions.demucs = query_python_version(&python_bin, "demucs").await;
    let _ = versions.save(&dirs.data_dir);

    Ok(())
}

/// Install PyTorch and torchcodec.
///
/// On Windows/Linux, installs CUDA-enabled PyTorch which automatically falls
/// back to CPU if no GPU is present. On macOS, installs the default (MPS-capable)
/// build from PyPI.
async fn install_torch(
    python_bin: &std::path::Path,
) -> Result<(), DependencyError> {
    if cfg!(target_os = "macos") {
        // macOS: install from PyPI (includes MPS support on Apple Silicon)
        tracing::info!("Installing PyTorch from PyPI (macOS)");
        let output = tokio::process::Command::new(python_bin)
            .args(["-m", "pip", "install", "--no-input", "torch"])
            .output()
            .await
            .map_err(DependencyError::Extract)?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(DependencyError::GitHubApi {
                message: format!("pip install torch failed: {stderr}"),
            });
        }
    } else {
        // Windows/Linux: install CUDA-enabled PyTorch (falls back to CPU automatically)
        let index_url = "https://download.pytorch.org/whl/cu126";
        tracing::info!("Installing CUDA-enabled PyTorch from {index_url}");

        let output = tokio::process::Command::new(python_bin)
            .args(["-m", "pip", "install", "--no-input", "torch", "--index-url", index_url])
            .output()
            .await
            .map_err(DependencyError::Extract)?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(DependencyError::GitHubApi {
                message: format!("pip install torch failed: {stderr}"),
            });
        }
    }

    // torchcodec from PyPI (works with any torch variant)
    let output = tokio::process::Command::new(python_bin)
        .args(["-m", "pip", "install", "--no-input", "torchcodec"])
        .output()
        .await
        .map_err(DependencyError::Extract)?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(DependencyError::GitHubApi {
            message: format!("pip install torchcodec failed: {stderr}"),
        });
    }

    Ok(())
}

/// Run `python --version` or similar to capture version string.
async fn query_version(bin: &std::path::Path, args: &[&str]) -> Option<String> {
    let output = tokio::process::Command::new(bin)
        .args(args)
        .output()
        .await
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    // "Python 3.13.12" → "3.13.12"
    text.split_whitespace().last().map(String::from)
}

/// Run `python -c "import <pkg>; print(<pkg>.__version__)"` to get package version.
async fn query_python_version(python_bin: &std::path::Path, package: &str) -> Option<String> {
    let code = format!("import {package}; print({package}.__version__)");
    let output = tokio::process::Command::new(python_bin)
        .args(["-c", &code])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Some(text.trim().to_string())
}
