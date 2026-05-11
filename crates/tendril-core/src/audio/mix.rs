use std::path::{Path, PathBuf};

use crate::config::OutputFormat;
use crate::error::AudioError;

use super::loudnorm::{self, LoudnormMeasurement};

/// Build the ffmpeg `-filter_complex` value used by `create_instrumental`.
///
/// `normalize=0` is the critical bit: it disables `amix`'s default 1/N divisor
/// and produces a true sample-accurate sum, which is what Demucs stems require
/// (vocals+drums+bass+other == original source). Without it, the instrumental
/// would be attenuated by 20*log10(3) ≈ 9.54 dB versus a true sum. Requires
/// ffmpeg >= 4.4; the bundled binary is well past that.
pub(crate) fn instrumental_filter() -> &'static str {
    "amix=inputs=3:duration=longest:normalize=0"
}

/// Create an instrumental mix by combining drums + bass + other stems,
/// two-pass loudness-normalized to `target_lufs`. Pass 1 sums the stems and
/// measures the result; pass 2 sums them again and applies a linear gain
/// adjustment based on the measurement. Falls back to single-pass loudnorm
/// on the sum if measurement parsing fails.
pub async fn create_instrumental(
    ffmpeg_bin: &Path,
    drums: &Path,
    bass: &Path,
    other: &Path,
    output_path: &Path,
    format: OutputFormat,
    target_lufs: f32,
) -> Result<PathBuf, AudioError> {
    let measurement = measure(ffmpeg_bin, drums, bass, other, target_lufs).await;
    let loudnorm_part = match &measurement {
        Some(m) => loudnorm::apply_filter(target_lufs, m),
        None => {
            tracing::warn!("loudnorm measurement failed for instrumental mix; falling back");
            loudnorm::loudnorm_filter(target_lufs)
        }
    };
    let filter = format!("{},{loudnorm_part}", instrumental_filter());

    let output = tokio::process::Command::new(ffmpeg_bin)
        .arg("-y")
        .arg("-i")
        .arg(drums)
        .arg("-i")
        .arg(bass)
        .arg("-i")
        .arg(other)
        .args(["-filter_complex", &filter])
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

/// Pass-1: sum the three stems and measure loudness. Output discarded.
async fn measure(
    ffmpeg_bin: &Path,
    drums: &Path,
    bass: &Path,
    other: &Path,
    target_lufs: f32,
) -> Option<LoudnormMeasurement> {
    let filter = format!(
        "{},{}",
        instrumental_filter(),
        loudnorm::measure_filter(target_lufs),
    );
    let out = tokio::process::Command::new(ffmpeg_bin)
        .arg("-i")
        .arg(drums)
        .arg("-i")
        .arg(bass)
        .arg("-i")
        .arg(other)
        .args(["-filter_complex", &filter])
        .args(["-f", "null", "-"])
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    loudnorm::parse_measurement(&String::from_utf8_lossy(&out.stderr))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instrumental_filter_disables_amix_normalization() {
        // The default amix=inputs=N divides the sum by N, which would attenuate
        // the instrumental by ~9.54 dB versus a true sum of the three demucs
        // stems. normalize=0 is what restores the sample-accurate sum.
        let filter = instrumental_filter();
        assert!(
            filter.contains("normalize=0"),
            "filter must disable amix normalization to avoid ~9.54 dB attenuation, got: {filter}"
        );
    }

    #[test]
    fn instrumental_filter_sums_three_inputs() {
        let filter = instrumental_filter();
        assert!(
            filter.contains("amix=inputs=3"),
            "filter must sum exactly 3 inputs (drums+bass+other), got: {filter}"
        );
    }

    #[test]
    fn instrumental_filter_uses_longest_duration() {
        // duration=longest avoids truncating output if any stem is a frame or
        // two longer than the others (demucs stems are usually but not always
        // bit-exactly the same length).
        let filter = instrumental_filter();
        assert!(
            filter.contains("duration=longest"),
            "filter must use longest input duration, got: {filter}"
        );
    }
}
