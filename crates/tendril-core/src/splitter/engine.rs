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
#[allow(clippy::too_many_arguments)]
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

    tokio::fs::create_dir_all(output_dir)
        .await
        .map_err(|e| SplitterError::Inference(format!("failed to create output dir: {e}")))?;

    tokio::fs::create_dir_all(models_dir)
        .await
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

    // We don't read demucs's stdout — pipe it to /dev/null so it can't fill
    // the OS pipe buffer (~64 KB on macOS, ~8 KB on Windows) and deadlock
    // the child waiting for someone to drain it. PyTorch warnings and inner
    // ffmpeg invocations occasionally land on stdout during long runs.
    cmd.stdout(std::process::Stdio::null());
    cmd.stderr(std::process::Stdio::piped());

    tracing::debug!(
        "Running demucs: model={model_name} device={gpu_backend:?} jobs={} input={} python={}",
        num_jobs(),
        input.display(),
        python_bin.display()
    );

    let mut child = cmd
        .spawn()
        .map_err(|e| SplitterError::Inference(format!("failed to spawn demucs: {e}")))?;

    // Capture PID up front for the Windows process-tree-kill path: once
    // child.wait() resolves, child.id() returns None.
    let child_pid = child.id();

    // Read stderr in a background task to parse progress
    let stderr = child.stderr.take().expect("stderr was piped above");
    let is_ft = model_name.contains("_ft");
    let progress_tx_clone = progress_tx.clone();

    let stderr_handle =
        tokio::spawn(async move { parse_stderr(stderr, is_ft, progress_tx_clone).await });

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
                kill_process_tree(&mut child, child_pid).await;
                let _ = child.wait().await;
                let _ = stderr_handle.await;
                return Err(SplitterError::Cancelled);
            }
        }
    } else {
        child
            .wait()
            .await
            .map_err(|e| SplitterError::Inference(format!("failed to wait on demucs: {e}")))?
    };

    let stderr_output = stderr_handle.await.unwrap_or_default();

    if !status.success() {
        // The returned error only carries the last 15 lines; log the full
        // demucs stderr at debug so the complete traceback (e.g. a Python
        // import/backend failure) is recoverable from the debug log.
        tracing::debug!("demucs failed; full stderr:\n{stderr_output}");
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

/// Best-effort process-tree termination for the demucs subprocess.
///
/// On Unix, `tokio::Child::kill` sends SIGKILL to the immediate child; demucs
/// rarely fans out beyond it, and any spawn_blocking ffmpeg helper exits when
/// its parent does. On Windows, the same call uses `TerminateProcess`, which
/// does NOT kill descendants — PyTorch DataLoader workers and the inner ffmpeg
/// would survive. We shell out to `taskkill /T /F /PID <pid>` first to walk
/// the tree, then fall back to `child.kill().await` to make sure the immediate
/// child is reaped on every platform.
async fn kill_process_tree(child: &mut tokio::process::Child, pid: Option<u32>) {
    #[cfg(windows)]
    if let Some(pid) = pid {
        let _ = tokio::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID"])
            .arg(pid.to_string())
            .output()
            .await;
    }

    #[cfg(not(windows))]
    let _ = pid;

    let _ = child.kill().await;
}

/// State machine for translating tqdm progress bytes into ProgressEvents.
///
/// Demucs emits progress like `\r  45%|████████░░░░░░░░|` on stderr. For
/// htdemucs_ft there are 4 separate passes (one per stem), each restarting
/// at 0%; we detect the wrap (`pct < last && last > 50`) to advance the
/// pass counter.
struct TqdmTracker {
    progress_re: Regex,
    is_ft: bool,
    ft_stems: u32,
    ft_stem_idx: u32,
    last_pct: f32,
}

impl TqdmTracker {
    fn new(is_ft: bool) -> Self {
        Self {
            progress_re: Regex::new(r"(\d+)%\|").expect("static regex compiles"),
            is_ft,
            ft_stems: if is_ft { 4 } else { 1 },
            ft_stem_idx: 0,
            last_pct: 0.0,
        }
    }

    /// Feed a chunk of stderr bytes and return the latest progress event,
    /// if the chunk contained at least one tqdm progress marker.
    fn feed(&mut self, chunk: &str) -> Option<ProgressEvent> {
        let caps = self.progress_re.captures_iter(chunk).last()?;
        let pct: f32 = caps[1].parse().ok()?;

        if self.is_ft && pct < self.last_pct && self.last_pct > 50.0 {
            self.ft_stem_idx = (self.ft_stem_idx + 1).min(self.ft_stems - 1);
        }
        self.last_pct = pct;

        let overall = (self.ft_stem_idx as f32 + pct / 100.0) / self.ft_stems as f32;
        let message = if self.is_ft {
            format!(
                "Separating stems (pass {}/{})... {:.0}%",
                self.ft_stem_idx + 1,
                self.ft_stems,
                pct
            )
        } else {
            format!("Separating stems... {pct:.0}%")
        };

        Some(ProgressEvent {
            stage: PipelineStage::Splitting,
            fraction: overall.min(0.95),
            message,
        })
    }
}

/// Parse tqdm progress bars from demucs stderr.
async fn parse_stderr(
    mut stderr: tokio::process::ChildStderr,
    is_ft: bool,
    progress_tx: Option<Arc<watch::Sender<ProgressEvent>>>,
) -> String {
    let mut tracker = TqdmTracker::new(is_ft);
    let mut all_output = String::new();
    let mut buf = [0u8; 4096];

    loop {
        match stderr.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                let chunk = String::from_utf8_lossy(&buf[..n]);
                all_output.push_str(&chunk);

                if let Some(tx) = &progress_tx
                    && let Some(event) = tracker.feed(&chunk)
                {
                    let _ = tx.send(event);
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
        .clamp(1, 4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tqdm_non_ft_basic_percentages() {
        let mut t = TqdmTracker::new(false);
        let e = t.feed("  0%|          | 0/10 [00:00<?, ?it/s]").unwrap();
        assert_eq!(e.stage, PipelineStage::Splitting);
        assert_eq!(e.fraction, 0.0);
        assert!(e.message.contains("0%"));

        let e = t
            .feed(" 60%|████████  | 6/10 [00:30<00:20, 1.2it/s]")
            .unwrap();
        assert!((e.fraction - 0.6).abs() < 1e-4);
        assert!(e.message.contains("60%"));

        let e = t
            .feed("100%|██████████| 10/10 [00:50<00:00, 1.2it/s]")
            .unwrap();
        // Even at 100%, overall is capped at 0.95 to leave headroom for the
        // postprocess/save step demucs runs after the last tqdm tick.
        assert!((e.fraction - 0.95).abs() < 1e-4);
        assert!(e.message.contains("100%"));
    }

    #[test]
    fn tqdm_ft_advances_pass_on_wraparound() {
        let mut t = TqdmTracker::new(true);

        let e = t.feed(" 80%|████████  | 8/10").unwrap();
        assert!(e.message.contains("pass 1/4"));
        // first quarter of 4 passes, capped: 0.80/4 = 0.20
        assert!((e.fraction - 0.20).abs() < 1e-4);

        // pct dropped from 80 → 5, last_pct > 50, so advance to pass 2.
        let e = t.feed("  5%|          | 0/10").unwrap();
        assert!(e.message.contains("pass 2/4"));
        // (1 + 0.05) / 4 = 0.2625
        assert!((e.fraction - 0.2625).abs() < 1e-4);

        // 95% on pass 2 → (1 + 0.95) / 4 = 0.4875
        let e = t.feed(" 95%|██████████|").unwrap();
        assert!(e.message.contains("pass 2/4"));
        assert!((e.fraction - 0.4875).abs() < 1e-4);

        let _ = t.feed(" 10%|").unwrap(); // → pass 3
        let _ = t.feed(" 90%|").unwrap();
        let e = t.feed("  0%|").unwrap(); // → pass 4
        assert!(e.message.contains("pass 4/4"));

        // last_pct was 0, so a further drop doesn't advance past 4.
        let e = t.feed("  0%|          |").unwrap();
        assert!(e.message.contains("pass 4/4"));
    }

    #[test]
    fn tqdm_ft_does_not_advance_on_small_dip() {
        // Within-pass jitter (e.g. 30% → 28% in a chunk boundary) must NOT
        // be misread as a pass boundary.
        let mut t = TqdmTracker::new(true);
        let _ = t.feed(" 30%|").unwrap();
        let e = t.feed(" 28%|").unwrap();
        // Still pass 1 — last_pct (30) was not > 50.
        assert!(e.message.contains("pass 1/4"));
    }

    #[test]
    fn tqdm_no_progress_in_chunk_returns_none() {
        let mut t = TqdmTracker::new(false);
        assert!(t.feed("nothing interesting here").is_none());
        assert!(t.feed("Some loaded warning from torch").is_none());
        // Lines that look superficially like progress but lack the trailing
        // `|` are also rejected by the regex.
        assert!(t.feed("loaded 50 weights").is_none());
    }

    #[test]
    fn tqdm_picks_last_marker_in_chunk() {
        // tqdm flushes carriage returns; a single read can include several.
        let mut t = TqdmTracker::new(false);
        let chunk = "\r 10%|█         |\r 20%|██        |\r 30%|███       |";
        let e = t.feed(chunk).unwrap();
        assert!(e.message.contains("30%"));
        assert!((e.fraction - 0.30).abs() < 1e-4);
    }

    #[test]
    fn tqdm_handles_zero_and_full_percent() {
        let mut t = TqdmTracker::new(false);
        let e = t.feed("  0%|          |").unwrap();
        assert!(e.message.contains("0%"));
        assert_eq!(e.fraction, 0.0);

        let e = t
            .feed("100%|██████████| 100/100 [01:00<00:00, 1.67it/s]")
            .unwrap();
        assert!(e.message.contains("100%"));
        assert!((e.fraction - 0.95).abs() < 1e-4);
    }
}
