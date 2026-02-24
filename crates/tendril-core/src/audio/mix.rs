use std::path::{Path, PathBuf};

use crate::config::OutputFormat;
use crate::error::AudioError;

/// Create an instrumental mix by combining drums + bass + other stems.
pub async fn create_instrumental(
    ffmpeg_bin: &Path,
    drums: &Path,
    bass: &Path,
    other: &Path,
    output_path: &Path,
    format: OutputFormat,
) -> Result<PathBuf, AudioError> {
    let output = tokio::process::Command::new(ffmpeg_bin)
        .arg("-y")
        .arg("-i")
        .arg(drums)
        .arg("-i")
        .arg(bass)
        .arg("-i")
        .arg(other)
        .args([
            "-filter_complex",
            "amix=inputs=3:duration=longest:normalize=0",
        ])
        .args(super::convert::codec_args(format))
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
