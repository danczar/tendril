use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};

use crate::error::YoutubeError;

/// How many trailing output lines to retain for diagnostics when yt-dlp fails
/// but writes its error to stdout (or nowhere obvious) instead of stderr.
const TAIL_LINES: usize = 40;

/// Download a YouTube video's audio as FLAC at 44.1 kHz using yt-dlp.
///
/// We keep a lossless copy of the decoded source so the optional preserved
/// full mix never goes through a lossy→lossy reencode, and we pin 44.1 kHz
/// to match Demucs's native rate so the full mix and instrumental line up.
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
    let output_path = output_dir.join(format!("{video_id}.flac"));
    let output_template = output_dir.join(format!("{video_id}.%(ext)s"));

    tracing::debug!("[yt-dlp] downloading {url}");

    let mut child = tokio::process::Command::new(ytdlp_bin)
        .args([
            "-f",
            "bestaudio",
            "-x",
            "--audio-format",
            "flac",
            "--audio-quality",
            "0",
            "--postprocessor-args",
            "ffmpeg:-ar 44100",
            "--no-playlist",
            "--ffmpeg-location",
            &ffmpeg_dir.to_string_lossy(),
            "-o",
            &output_template.to_string_lossy(),
            &url,
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| YoutubeError::Download {
            url: url.clone(),
            message: e.to_string(),
        })?;

    // Stream both pipes line-by-line so yt-dlp's progress and errors show up
    // live at debug level (instead of being buffered until exit), and so the
    // pipe buffers never fill and stall the child. We keep the full stderr and
    // a tail of stdout to build a useful error message — yt-dlp writes some
    // fatal errors to stdout, so stderr alone is often empty.
    let stdout_task = child.stdout.take().map(|s| {
        tokio::spawn(async move {
            let mut tail: Vec<String> = Vec::new();
            let mut lines = BufReader::new(s).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!("[yt-dlp] {line}");
                tail.push(line);
                if tail.len() > TAIL_LINES {
                    tail.remove(0);
                }
            }
            tail
        })
    });

    let stderr_task = child.stderr.take().map(|s| {
        tokio::spawn(async move {
            let mut buf = String::new();
            let mut lines = BufReader::new(s).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                tracing::debug!("[yt-dlp] stderr: {line}");
                buf.push_str(&line);
                buf.push('\n');
            }
            buf
        })
    });

    let status = child.wait().await.map_err(|e| YoutubeError::Download {
        url: url.clone(),
        message: e.to_string(),
    })?;

    let stdout_tail = match stdout_task {
        Some(t) => t.await.unwrap_or_default(),
        None => Vec::new(),
    };
    let stderr_buf = match stderr_task {
        Some(t) => t.await.unwrap_or_default(),
        None => String::new(),
    };

    if !status.success() {
        // Prefer stderr, but fall back to the stdout tail when yt-dlp logged
        // its error there — otherwise the user sees an exit code and nothing else.
        let detail = if stderr_buf.trim().is_empty() {
            stdout_tail.join("\n")
        } else {
            stderr_buf
        };
        return Err(YoutubeError::Process {
            code: status.code().unwrap_or(-1),
            stderr: detail.trim().to_string(),
        });
    }

    if output_path.exists() {
        Ok(output_path)
    } else {
        let tail = stdout_tail.join("\n");
        Err(YoutubeError::Download {
            url,
            message: format!(
                "FLAC file not found after download — ffmpeg conversion may have failed.\n\
                 Last yt-dlp output:\n{tail}"
            ),
        })
    }
}
