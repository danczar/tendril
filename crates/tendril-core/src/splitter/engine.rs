use std::path::{Path, PathBuf};
use std::sync::Arc;

use regex::Regex;
use tokio::io::AsyncReadExt;
use tokio::sync::watch;

use crate::config::GpuBackend;
use crate::error::SplitterError;
use crate::progress::{PipelineStage, ProgressEvent};

/// Output stems from separation.
pub struct StemOutput {
    pub vocals: PathBuf,
    pub drums: PathBuf,
    pub bass: PathBuf,
    pub other: PathBuf,
}

/// Run Demucs stem separation via a bundled Python subprocess.
///
/// Invokes `python -m demucs.separate` with MPS/CUDA/CPU device selection.
/// Progress is parsed from tqdm output on stderr.
pub async fn separate(
    input: &Path,
    output_dir: &Path,
    model_name: &str,
    gpu_backend: GpuBackend,
    python_bin: &Path,
    models_dir: &Path,
    bin_dir: &Path,
    progress_tx: Option<Arc<watch::Sender<ProgressEvent>>>,
    cancel_rx: Option<watch::Receiver<bool>>,
) -> Result<StemOutput, SplitterError> {
    if !python_bin.exists() {
        return Err(SplitterError::Inference(format!(
            "Python not found at {} — install dependencies first",
            python_bin.display()
        )));
    }

    std::fs::create_dir_all(output_dir)
        .map_err(|e| SplitterError::Inference(format!("failed to create output dir: {e}")))?;

    std::fs::create_dir_all(models_dir)
        .map_err(|e| SplitterError::Inference(format!("failed to create models dir: {e}")))?;

    let mut cmd = tokio::process::Command::new(python_bin);
    cmd.arg("-m").arg("demucs.separate");
    cmd.arg(input);
    cmd.arg("-n").arg(model_name);
    cmd.arg("-o").arg(output_dir);
    cmd.arg("-j").arg(num_jobs().to_string());

    // Point torch hub cache into our managed data directory so models
    // don't scatter into the user's home folder.
    cmd.env("TORCH_HOME", models_dir);

    // Put our managed bin_dir (ffmpeg, ffprobe) on PATH so demucs can
    // use ffmpeg/ffprobe for audio loading without a system install.
    if bin_dir.exists() {
        let mut path = std::ffi::OsString::from(bin_dir);
        if let Some(existing) = std::env::var_os("PATH") {
            path.push(if cfg!(windows) { ";" } else { ":" });
            path.push(existing);
        }
        cmd.env("PATH", path);
    }

    // Device selection — Auto picks the best available accelerator per platform
    match gpu_backend {
        GpuBackend::Auto => {
            if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
                cmd.arg("-d").arg("mps");
            }
            // On Windows/Linux, omit -d so demucs auto-detects CUDA if
            // GPU-enabled PyTorch is installed, otherwise falls back to CPU.
        }
        GpuBackend::Mps => {
            cmd.arg("-d").arg("mps");
        }
        GpuBackend::Cuda => {
            cmd.arg("-d").arg("cuda");
        }
        GpuBackend::Cpu => {
            cmd.arg("-d").arg("cpu");
        }
    }

    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| SplitterError::Inference(format!("failed to spawn demucs: {e}")))?;

    // Read stderr in a background task to parse progress
    let stderr = child.stderr.take().unwrap();
    let is_ft = model_name.contains("_ft");
    let progress_tx_clone = progress_tx.clone();

    let stderr_handle = tokio::spawn(async move {
        parse_stderr(stderr, is_ft, progress_tx_clone).await
    });

    let status = if let Some(mut rx) = cancel_rx {
        tokio::select! {
            result = child.wait() => {
                result.map_err(|e| SplitterError::Inference(format!("failed to wait on demucs: {e}")))?
            }
            _ = async {
                while !*rx.borrow() {
                    if rx.changed().await.is_err() { break; }
                }
            } => {
                let _ = child.kill().await;
                return Err(SplitterError::Inference("cancelled".into()));
            }
        }
    } else {
        child.wait().await
            .map_err(|e| SplitterError::Inference(format!("failed to wait on demucs: {e}")))?
    };

    let stderr_output = stderr_handle.await.unwrap_or_default();

    if !status.success() {
        let last_lines: String = stderr_output
            .lines()
            .rev()
            .take(15)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        return Err(SplitterError::Inference(format!(
            "demucs exited with code {}: {}",
            status.code().unwrap_or(-1),
            last_lines
        )));
    }

    // Demucs writes to: <output_dir>/<model_name>/<input_stem>/
    let input_stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    let demucs_output = output_dir.join(model_name).join(input_stem);

    let stems = StemOutput {
        vocals: demucs_output.join("vocals.wav"),
        drums: demucs_output.join("drums.wav"),
        bass: demucs_output.join("bass.wav"),
        other: demucs_output.join("other.wav"),
    };

    // Verify outputs exist
    for (name, path) in [
        ("vocals", &stems.vocals),
        ("drums", &stems.drums),
        ("bass", &stems.bass),
        ("other", &stems.other),
    ] {
        if !path.exists() {
            return Err(SplitterError::Inference(format!(
                "demucs output not found: {} (expected at {})",
                name,
                path.display()
            )));
        }
    }

    Ok(stems)
}

/// Parse tqdm progress bars from demucs stderr.
///
/// Demucs outputs progress like: `\r  45%|████████░░░░░░░░|`
/// For htdemucs_ft, there are 4 separate passes (one per stem).
async fn parse_stderr(
    mut stderr: tokio::process::ChildStderr,
    is_ft: bool,
    progress_tx: Option<Arc<watch::Sender<ProgressEvent>>>,
) -> String {
    let progress_re = Regex::new(r"(\d+)%\|").unwrap();
    let mut all_output = String::new();
    let mut buf = [0u8; 4096];
    let mut ft_stem_idx: u32 = 0;
    let mut last_pct: f32 = 0.0;
    let ft_stems: u32 = if is_ft { 4 } else { 1 };

    loop {
        match stderr.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                let chunk = String::from_utf8_lossy(&buf[..n]);
                all_output.push_str(&chunk);

                if let Some(tx) = &progress_tx {
                    // Find the last progress match in this chunk
                    if let Some(caps) = progress_re.captures_iter(&chunk).last() {
                        if let Ok(pct) = caps[1].parse::<f32>() {
                            // Detect stem transition for htdemucs_ft
                            if is_ft && pct < last_pct && last_pct > 50.0 {
                                ft_stem_idx = (ft_stem_idx + 1).min(ft_stems - 1);
                            }
                            last_pct = pct;

                            let overall = (ft_stem_idx as f32 + pct / 100.0) / ft_stems as f32;
                            let _ = tx.send(ProgressEvent {
                                stage: PipelineStage::Splitting,
                                fraction: overall.min(0.95), // cap at 95% until done
                                message: if is_ft {
                                    format!(
                                        "Separating stems (pass {}/{})... {:.0}%",
                                        ft_stem_idx + 1,
                                        ft_stems,
                                        pct
                                    )
                                } else {
                                    format!("Separating stems... {:.0}%", pct)
                                },
                            });
                        }
                    }
                }
            }
            Err(_) => break,
        }
    }

    all_output
}

/// Compute job count for demucs parallel processing.
fn num_jobs() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(4)
        .max(1)
}
