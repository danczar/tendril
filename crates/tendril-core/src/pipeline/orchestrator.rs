use std::path::Path;
use std::sync::Arc;

use futures::future::try_join_all;
use tokio::sync::watch;

use crate::config::{GpuBackend, OutputFormat};
use crate::error::{PipelineError, SplitterError};
use crate::pipeline::job::JobSource;
use crate::progress::{PipelineStage, ProgressEvent};

/// All paths and config the pipeline needs to run a job.
pub struct PipelineContext {
    pub ytdlp_bin: std::path::PathBuf,
    pub ffmpeg_bin: std::path::PathBuf,
    pub python_bin: std::path::PathBuf,
    pub models_dir: std::path::PathBuf,
    pub cache_dir: std::path::PathBuf,
    pub output_dir: std::path::PathBuf,
    pub output_format: OutputFormat,
    pub gpu_backend: GpuBackend,
    pub model_name: String,
    pub preserve_full_mix: bool,
}

/// Run the full processing pipeline for a single job:
/// download → split → convert → mix.
pub async fn run(
    ctx: &PipelineContext,
    source: JobSource,
    progress_tx: Arc<watch::Sender<ProgressEvent>>,
    cancel_rx: watch::Receiver<bool>,
) -> Result<(), PipelineError> {
    let send = |stage, fraction, message: &str| {
        let _ = progress_tx.send(ProgressEvent {
            stage,
            fraction,
            message: message.to_string(),
        });
    };

    // ── Stage 1: Download (YouTube only) ──
    let audio_path = match &source {
        JobSource::Youtube { video_id, title } => {
            send(
                PipelineStage::Downloading,
                0.0,
                &format!("Downloading {title}..."),
            );

            let download_dir = ctx.cache_dir.join("downloads");
            let ffmpeg_dir = ctx
                .ffmpeg_bin
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .to_path_buf();

            let path = crate::youtube::download::download_audio(
                &ctx.ytdlp_bin,
                &ffmpeg_dir,
                video_id,
                &download_dir,
            )
            .await
            .map_err(|e| PipelineError::StageFailed {
                stage: "download".into(),
                message: e.to_string(),
            })?;

            check_cancelled(&cancel_rx)?;
            send(PipelineStage::Downloading, 1.0, "Download complete");
            path
        }
        JobSource::LocalFile { path } => path.clone(),
    };

    // Build the final output dir up front so we can write the full mix
    // before the (long) splitting stage runs.
    let song_name =
        crate::pipeline::job::output_folder_name(source.display_name(), source.video_id());
    let final_dir = ctx.output_dir.join(&song_name);
    tokio::fs::create_dir_all(&final_dir)
        .await
        .map_err(|e| PipelineError::StageFailed {
            stage: "convert".into(),
            message: e.to_string(),
        })?;

    let ext = ctx.output_format.extension();

    // ── Preserve full mix (optional) ──
    // Written first so the user has a usable artifact even if a later
    // stage fails or is cancelled. Source is the lossless download (or
    // the user's original local file), so this is a clean reencode.
    if ctx.preserve_full_mix {
        check_cancelled(&cancel_rx)?;
        send(PipelineStage::Downloading, 1.0, "Saving full mix...");

        let full_mix_path = final_dir.join(format!("full_mix.{ext}"));
        crate::audio::convert::convert_to(
            &ctx.ffmpeg_bin,
            &audio_path,
            ctx.output_format,
            &full_mix_path,
        )
        .await
        .map_err(|e| PipelineError::StageFailed {
            stage: "convert".into(),
            message: e.to_string(),
        })?;

        check_cancelled(&cancel_rx)?;
    }

    // ── Stage 2: Split stems ──
    send(PipelineStage::Splitting, 0.0, "Separating stems...");

    let stem_dir = ctx.cache_dir.join("stems").join(
        audio_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown"),
    );

    let bin_dir = ctx
        .ffmpeg_bin
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();

    let stems = crate::splitter::engine::separate(
        &audio_path,
        &stem_dir,
        &ctx.model_name,
        ctx.gpu_backend,
        &ctx.python_bin,
        &ctx.models_dir,
        &bin_dir,
        Some(progress_tx.clone()),
        Some(cancel_rx.clone()),
    )
    .await
    .map_err(|e| match e {
        SplitterError::Cancelled => PipelineError::Cancelled,
        other => PipelineError::StageFailed {
            stage: "split".into(),
            message: other.to_string(),
        },
    })?;

    check_cancelled(&cancel_rx)?;

    // ── Stage 3: Convert stems to output format ──
    send(
        PipelineStage::Converting,
        0.0,
        "Converting to output format...",
    );

    let stem_paths = [&stems.vocals, &stems.drums, &stems.bass, &stems.other];

    // Each ffmpeg invocation is single-threaded for typical stem codecs, so
    // run all 4 concurrently (~10s saved per job on multi-core machines).
    // Progress is reported once at start and once after the batch completes
    // rather than per-stem, since per-stem completion ordering with parallel
    // futures isn't worth the extra plumbing for a few-second operation.
    send(PipelineStage::Converting, 0.0, "Converting 4 stems...");

    let convert_futs = stem_paths.iter().map(|stem_path| {
        crate::audio::convert::convert(&ctx.ffmpeg_bin, stem_path, ctx.output_format, &final_dir)
    });

    try_join_all(convert_futs)
        .await
        .map_err(|e| PipelineError::StageFailed {
            stage: "convert".into(),
            message: e.to_string(),
        })?;

    // Cancellation is checked once after the batch completes; a cancel signal
    // arriving mid-batch lets the in-flight ffmpegs run a few extra seconds
    // before we honor it. Acceptable tradeoff for the simpler control flow.
    check_cancelled(&cancel_rx)?;

    send(PipelineStage::Converting, 1.0, "Converted 4/4");

    // ── Stage 4: Create instrumental mix ──
    send(PipelineStage::Mixing, 0.0, "Creating instrumental mix...");

    let instrumental_path = final_dir.join(format!("instrumental.{ext}"));

    crate::audio::mix::create_instrumental(
        &ctx.ffmpeg_bin,
        &stems.drums,
        &stems.bass,
        &stems.other,
        &instrumental_path,
        ctx.output_format,
    )
    .await
    .map_err(|e| PipelineError::StageFailed {
        stage: "mix".into(),
        message: e.to_string(),
    })?;

    // ── Done ──
    send(PipelineStage::Complete, 1.0, "Done!");

    // ── Clean up temp files ──
    cleanup_temp_files(&audio_path, &stem_dir, &source).await;

    Ok(())
}

/// Check if the job has been cancelled.
fn check_cancelled(cancel_rx: &watch::Receiver<bool>) -> Result<(), PipelineError> {
    if *cancel_rx.borrow() {
        return Err(PipelineError::Cancelled);
    }
    Ok(())
}

/// Remove intermediate downloads and stem files after successful completion.
async fn cleanup_temp_files(audio_path: &Path, stem_dir: &Path, source: &JobSource) {
    // Only delete downloaded files for YouTube sources (not user's local files)
    if matches!(source, JobSource::Youtube { .. })
        && let Err(e) = tokio::fs::remove_file(audio_path).await
    {
        tracing::warn!("Failed to clean up download {}: {e}", audio_path.display());
    }

    // Clean up intermediate stem WAVs
    if stem_dir.exists()
        && let Err(e) = tokio::fs::remove_dir_all(stem_dir).await
    {
        tracing::warn!("Failed to clean up stems {}: {e}", stem_dir.display());
    }
}
