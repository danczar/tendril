use std::path::Path;
use std::sync::Arc;

use tokio::sync::watch;

use crate::config::{GpuBackend, OutputFormat};
use crate::error::PipelineError;
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
    mut cancel_rx: watch::Receiver<bool>,
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
                .unwrap_or(Path::new("."))
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

            check_cancelled(&mut cancel_rx)?;
            send(PipelineStage::Downloading, 1.0, "Download complete");
            path
        }
        JobSource::LocalFile { path } => path.clone(),
    };

    // ── Stage 2: Split stems ──
    send(PipelineStage::Splitting, 0.0, "Separating stems...");

    let stem_dir = ctx.cache_dir.join("stems").join(
        audio_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown"),
    );

    let stems = crate::splitter::engine::separate(
        &audio_path,
        &stem_dir,
        &ctx.model_name,
        ctx.gpu_backend,
        &ctx.python_bin,
        &ctx.models_dir,
        Some(progress_tx.clone()),
        Some(cancel_rx.clone()),
    )
    .await
    .map_err(|e| PipelineError::StageFailed {
        stage: "split".into(),
        message: e.to_string(),
    })?;

    check_cancelled(&mut cancel_rx)?;

    // ── Stage 3: Convert stems to output format ──
    send(
        PipelineStage::Converting,
        0.0,
        "Converting to output format...",
    );

    let song_name = crate::pipeline::job::output_folder_name(
        source.display_name(),
        source.video_id(),
    );
    let final_dir = ctx.output_dir.join(&song_name);
    std::fs::create_dir_all(&final_dir).map_err(|e| PipelineError::StageFailed {
        stage: "convert".into(),
        message: e.to_string(),
    })?;

    let stem_paths = [&stems.vocals, &stems.drums, &stems.bass, &stems.other];

    for (i, stem_path) in stem_paths.iter().enumerate() {
        crate::audio::convert::convert(
            &ctx.ffmpeg_bin,
            stem_path,
            ctx.output_format,
            &final_dir,
        )
        .await
        .map_err(|e| PipelineError::StageFailed {
            stage: "convert".into(),
            message: e.to_string(),
        })?;

        check_cancelled(&mut cancel_rx)?;

        send(
            PipelineStage::Converting,
            (i + 1) as f32 / 4.0,
            &format!("Converted {}/{}", i + 1, 4),
        );
    }

    // ── Stage 4: Create instrumental mix ──
    send(PipelineStage::Mixing, 0.0, "Creating instrumental mix...");

    let ext = ctx.output_format.extension();
    let instrumental_path = final_dir.join(format!("instrumental.{ext}"));

    // ── Preserve full mix (optional) ──
    if ctx.preserve_full_mix {
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
    }

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
    cleanup_temp_files(&audio_path, &stem_dir, &source);

    Ok(())
}

/// Check if the job has been cancelled.
fn check_cancelled(cancel_rx: &mut watch::Receiver<bool>) -> Result<(), PipelineError> {
    if *cancel_rx.borrow() {
        return Err(PipelineError::Cancelled);
    }
    Ok(())
}

/// Remove intermediate downloads and stem files after successful completion.
fn cleanup_temp_files(audio_path: &Path, stem_dir: &Path, source: &JobSource) {
    // Only delete downloaded files for YouTube sources (not user's local files)
    if matches!(source, JobSource::Youtube { .. }) {
        if let Err(e) = std::fs::remove_file(audio_path) {
            tracing::warn!("Failed to clean up download {}: {e}", audio_path.display());
        }
    }

    // Clean up intermediate stem WAVs
    if stem_dir.exists() {
        if let Err(e) = std::fs::remove_dir_all(stem_dir) {
            tracing::warn!("Failed to clean up stems {}: {e}", stem_dir.display());
        }
    }
}
