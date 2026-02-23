use std::path::{Path, PathBuf};

use crate::error::YoutubeError;

/// Download a YouTube video's audio as WAV using yt-dlp.
///
/// Grabs the highest quality audio-only stream and converts to WAV
/// so stem-splitter-core can decode it without needing Opus/AAC codecs.
pub async fn download_audio(
    ytdlp_bin: &Path,
    ffmpeg_dir: &Path,
    video_id: &str,
    output_dir: &Path,
) -> Result<PathBuf, YoutubeError> {
    std::fs::create_dir_all(output_dir).map_err(|e| YoutubeError::Download {
        url: video_id.to_string(),
        message: e.to_string(),
    })?;

    let url = format!("https://www.youtube.com/watch?v={video_id}");
    let output_path = output_dir.join(format!("{video_id}.wav"));
    let output_template = output_dir.join(format!("{video_id}.%(ext)s"));

    let output = tokio::process::Command::new(ytdlp_bin)
        .args([
            "-f", "bestaudio",
            "-x",
            "--audio-format", "wav",
            "--audio-quality", "0",
            "--no-playlist",
            "--ffmpeg-location",
            &ffmpeg_dir.to_string_lossy(),
            "-o",
            &output_template.to_string_lossy(),
            &url,
        ])
        .output()
        .await
        .map_err(|e| YoutubeError::Download {
            url: url.clone(),
            message: e.to_string(),
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(YoutubeError::Process {
            code: output.status.code().unwrap_or(-1),
            stderr,
        });
    }

    if output_path.exists() {
        Ok(output_path)
    } else {
        Err(YoutubeError::Download {
            url,
            message: "WAV file not found after download — ffmpeg conversion may have failed".into(),
        })
    }
}
