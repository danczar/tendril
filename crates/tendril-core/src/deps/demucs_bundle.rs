use std::path::PathBuf;
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};
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
    {
        "aarch64-apple-darwin"
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        "x86_64-apple-darwin"
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        "x86_64-pc-windows-msvc"
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "x86_64-unknown-linux-gnu"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "aarch64-unknown-linux-gnu"
    }
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
        return install_demucs(dirs, &python_bin, progress_tx.as_ref()).await;
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
    tokio::fs::create_dir_all(&demucs_dir)
        .await
        .map_err(DependencyError::Extract)?;

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
    .map_err(|e| DependencyError::Extract(std::io::Error::other(e)))?
    .map_err(DependencyError::Extract)?;

    // Set executable permissions
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if python_bin.exists() {
            tokio::fs::set_permissions(&python_bin, std::fs::Permissions::from_mode(0o755))
                .await
                .map_err(DependencyError::Extract)?;
        }
    }

    if !python_bin.exists() {
        return Err(DependencyError::BinaryNotFound {
            path: python_bin.clone(),
        });
    }

    // ── Step 3: pip install torch + demucs ──
    send(
        "PyTorch",
        0.35,
        "Installing PyTorch (this may take a few minutes)...",
    );

    install_torch(&python_bin, progress_tx.as_ref()).await?;

    send("demucs", 0.75, "Installing demucs...");
    tracing::info!("Installing demucs...");

    run_pip_install_streaming(
        &python_bin,
        &["demucs", "soundfile"],
        "demucs",
        progress_tx.as_ref(),
        0.75,
    )
    .await?;

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
    progress_tx: Option<&watch::Sender<DownloadProgress>>,
) -> Result<PathBuf, DependencyError> {
    if let Some(tx) = progress_tx {
        let _ = tx.send(DownloadProgress {
            tool: "demucs".into(),
            fraction: 0.5,
            message: "Installing demucs...".into(),
        });
    }

    run_pip_install_streaming(
        python_bin,
        &["demucs", "soundfile"],
        "demucs",
        progress_tx,
        0.5,
    )
    .await?;

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

    run_pip_install_streaming(&python_bin, &["--upgrade", "demucs"], "demucs", None, 0.0).await?;

    let mut versions = InstalledVersions::load(&dirs.data_dir);
    versions.demucs = query_python_version(&python_bin, "demucs").await;
    let _ = versions.save(&dirs.data_dir);

    Ok(())
}

/// Pinned PyTorch / torchaudio versions.
///
/// These are deliberately pinned rather than floating. torchaudio >= 2.9 routes
/// all audio I/O through `torchcodec`, which loads ffmpeg **shared libraries**
/// at runtime — making demucs depend on whatever ffmpeg the host happens to
/// have installed (and break when, say, a `brew upgrade` bumps it to an
/// unsupported major or removes a linked dylib). 2.7.x is the last line that
/// uses torchaudio's native `soundfile` (libsndfile) backend for WAV I/O, which
/// is fully self-contained and needs no ffmpeg at all. demucs only reads the
/// input via the bundled ffmpeg binary and writes WAV stems via `ta.save`, so
/// soundfile covers the write path completely.
const TORCH_PIN: &str = "torch==2.7.1";
const TORCHAUDIO_PIN: &str = "torchaudio==2.7.1";

/// Install PyTorch, torchaudio, and the soundfile audio backend.
///
/// On Windows/Linux, installs CUDA-enabled PyTorch which automatically falls
/// back to CPU if no GPU is present. On macOS, installs the default (MPS-capable)
/// build from PyPI. torchcodec is intentionally NOT installed — see `TORCH_PIN`.
async fn install_torch(
    python_bin: &std::path::Path,
    progress_tx: Option<&watch::Sender<DownloadProgress>>,
) -> Result<(), DependencyError> {
    if cfg!(target_os = "macos") {
        // macOS: install from PyPI (includes MPS support on Apple Silicon)
        tracing::info!("Installing {TORCH_PIN} + {TORCHAUDIO_PIN} from PyPI (macOS)");
        run_pip_install_streaming(
            python_bin,
            &[TORCH_PIN, TORCHAUDIO_PIN],
            "PyTorch",
            progress_tx,
            0.4,
        )
        .await?;
    } else {
        // Windows/Linux: install CUDA-enabled PyTorch (falls back to CPU automatically)
        let index_url = "https://download.pytorch.org/whl/cu126";
        tracing::info!("Installing CUDA-enabled {TORCH_PIN} + {TORCHAUDIO_PIN} from {index_url}");
        run_pip_install_streaming(
            python_bin,
            &[TORCH_PIN, TORCHAUDIO_PIN, "--index-url", index_url],
            "PyTorch",
            progress_tx,
            0.4,
        )
        .await?;
    }

    // soundfile (libsndfile) is torchaudio's WAV backend and replaces torchcodec
    // entirely. Installed from PyPI (the CUDA index above doesn't carry it).
    tracing::info!("Installing soundfile (libsndfile audio backend)");
    run_pip_install_streaming(python_bin, &["soundfile"], "PyTorch", progress_tx, 0.7).await?;

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

/// Run `python -m pip install ...` and stream stdout/stderr line-by-line,
/// forwarding any "Downloading <wheel>" / "Collecting <pkg>" lines as
/// progress messages so the UI stops looking frozen during the
/// multi-minute torch install.
///
/// Returns Ok on success, Err with the captured stderr on failure.
async fn run_pip_install_streaming(
    python_bin: &std::path::Path,
    args: &[&str],
    tool_label: &str,
    progress_tx: Option<&watch::Sender<DownloadProgress>>,
    base_fraction: f32,
) -> Result<(), DependencyError> {
    let mut full_args = vec!["-m", "pip", "install", "--no-input"];
    full_args.extend_from_slice(args);

    let mut child = tokio::process::Command::new(python_bin)
        .args(&full_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(DependencyError::Extract)?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // Spawn a task per stream so they drain in parallel and pip's
    // pipe buffers never block the child.
    let tool_for_out = tool_label.to_string();
    let tx_for_out = progress_tx.cloned();
    let stdout_task = tokio::spawn(async move {
        if let Some(s) = stdout {
            let mut lines = BufReader::new(s).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "deps::pip", "{line}");
                forward_pip_line(&line, &tool_for_out, tx_for_out.as_ref(), base_fraction);
            }
        }
    });

    let mut stderr_buf = String::new();
    let stderr_task = tokio::spawn(async move {
        let mut buf = String::new();
        if let Some(s) = stderr {
            let mut lines = BufReader::new(s).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!(target: "deps::pip", "stderr: {line}");
                buf.push_str(&line);
                buf.push('\n');
            }
        }
        buf
    });

    let status = child.wait().await.map_err(DependencyError::Extract)?;
    let _ = stdout_task.await;
    if let Ok(buf) = stderr_task.await {
        stderr_buf = buf;
    }

    if !status.success() {
        return Err(DependencyError::GitHubApi {
            message: format!(
                "pip install {} failed: {}",
                args.join(" "),
                stderr_buf.trim()
            ),
        });
    }

    Ok(())
}

/// Inspect a pip stdout line and forward useful progress info to the UI.
fn forward_pip_line(
    line: &str,
    tool: &str,
    tx: Option<&watch::Sender<DownloadProgress>>,
    base_fraction: f32,
) {
    let trimmed = line.trim();
    let interesting = trimmed.starts_with("Downloading ")
        || trimmed.starts_with("Collecting ")
        || trimmed.starts_with("Installing collected packages");

    if !interesting {
        return;
    }
    if let Some(tx) = tx {
        let _ = tx.send(DownloadProgress {
            tool: tool.into(),
            fraction: base_fraction,
            message: trimmed.to_string(),
        });
    }
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
