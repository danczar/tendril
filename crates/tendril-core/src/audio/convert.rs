use std::path::{Path, PathBuf};

use crate::config::OutputFormat;
use crate::error::AudioError;

/// Codec arguments for each output format.
pub(crate) fn codec_args(format: OutputFormat) -> &'static [&'static str] {
    match format {
        OutputFormat::Wav => &[],
        OutputFormat::Flac => &["-c:a", "flac", "-compression_level", "0"],
        OutputFormat::Mp3 => &["-c:a", "libmp3lame", "-q:a", "0"],
        OutputFormat::Aac => &["-c:a", "aac", "-b:a", "320k"],
    }
}

/// Convert an audio file to the target format using ffmpeg.
pub async fn convert(
    ffmpeg_bin: &Path,
    input: &Path,
    format: OutputFormat,
    output_dir: &Path,
) -> Result<PathBuf, AudioError> {
    let stem = input
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");

    let ext = format.extension();
    let output_path = output_dir.join(format!("{stem}.{ext}"));

    // If target is WAV and input is already WAV, just copy
    if format == OutputFormat::Wav
        && input.extension().and_then(|e| e.to_str()) == Some("wav")
    {
        std::fs::copy(input, &output_path).map_err(|e| AudioError::Conversion {
            message: format!("failed to copy WAV: {e}"),
        })?;
        return Ok(output_path);
    }

    let output = tokio::process::Command::new(ffmpeg_bin)
        .arg("-y")
        .arg("-i")
        .arg(input)
        .arg("-vn")
        .args(codec_args(format))
        .arg(&output_path)
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

    Ok(output_path)
}

/// Convert an audio file to the target format, writing to an explicit output path.
pub async fn convert_to(
    ffmpeg_bin: &Path,
    input: &Path,
    format: OutputFormat,
    output_path: &Path,
) -> Result<PathBuf, AudioError> {
    if format == OutputFormat::Wav
        && input.extension().and_then(|e| e.to_str()) == Some("wav")
    {
        std::fs::copy(input, output_path).map_err(|e| AudioError::Conversion {
            message: format!("failed to copy WAV: {e}"),
        })?;
        return Ok(output_path.to_path_buf());
    }

    let output = tokio::process::Command::new(ffmpeg_bin)
        .arg("-y")
        .arg("-i")
        .arg(input)
        .arg("-vn")
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
