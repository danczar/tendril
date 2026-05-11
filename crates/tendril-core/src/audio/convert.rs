use std::path::{Path, PathBuf};

use crate::config::OutputFormat;
use crate::error::AudioError;

use super::loudnorm::{self, LoudnormMeasurement};

/// Codec arguments for each output format.
pub(crate) const fn codec_args(format: OutputFormat) -> &'static [&'static str] {
    match format {
        OutputFormat::Wav => &[],
        OutputFormat::Flac => &["-c:a", "flac", "-compression_level", "0"],
        OutputFormat::Mp3 => &["-c:a", "libmp3lame", "-q:a", "0"],
        OutputFormat::Aac => &["-c:a", "aac", "-b:a", "320k"],
    }
}

/// Convert an audio file to the target format, two-pass loudness-normalized
/// to `target_lufs`.
pub async fn convert(
    ffmpeg_bin: &Path,
    input: &Path,
    format: OutputFormat,
    output_dir: &Path,
    target_lufs: f32,
) -> Result<PathBuf, AudioError> {
    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");

    let output_path = output_dir.join(format!("{stem}.{}", format.extension()));
    convert_to(ffmpeg_bin, input, format, &output_path, target_lufs).await
}

/// Convert an audio file to the target format, writing to an explicit output
/// path. Runs two-pass loudnorm (measure → linear apply) for mastering-grade
/// accuracy; falls back to single-pass if measurement parsing fails (silent
/// input, ffmpeg version drift, etc.).
pub async fn convert_to(
    ffmpeg_bin: &Path,
    input: &Path,
    format: OutputFormat,
    output_path: &Path,
    target_lufs: f32,
) -> Result<PathBuf, AudioError> {
    let measurement = measure(ffmpeg_bin, input, target_lufs).await;
    let filter = match &measurement {
        Some(m) => loudnorm::apply_filter(target_lufs, m),
        None => {
            tracing::warn!(
                "loudnorm measurement failed for {}; falling back to single-pass",
                input.display()
            );
            loudnorm::loudnorm_filter(target_lufs)
        }
    };

    let output = tokio::process::Command::new(ffmpeg_bin)
        .arg("-y")
        .arg("-i")
        .arg(input)
        .arg("-vn")
        .args(["-af", &filter])
        .args(codec_args(format))
        .arg(output_path)
        .output()
        .await
        .map_err(|e| AudioError::Conversion {
            message: e.to_string(),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(AudioError::FfmpegProcess {
            code: output.status.code().unwrap_or(-1),
            stderr,
        });
    }

    Ok(output_path.to_path_buf())
}

/// Pass-1: run loudnorm in measure-only mode and parse the JSON summary.
/// Output is discarded via `-f null -`.
async fn measure(ffmpeg_bin: &Path, input: &Path, target_lufs: f32) -> Option<LoudnormMeasurement> {
    let filter = loudnorm::measure_filter(target_lufs);
    let out = tokio::process::Command::new(ffmpeg_bin)
        .arg("-i")
        .arg(input)
        .arg("-vn")
        .args(["-af", &filter])
        .args(["-f", "null", "-"])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    loudnorm::parse_measurement(&String::from_utf8_lossy(&out.stderr))
}
