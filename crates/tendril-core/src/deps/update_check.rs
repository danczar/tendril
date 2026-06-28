use crate::deps::github_release;

/// Check the latest demucs version on PyPI.
pub async fn check_demucs_latest(client: &reqwest::Client) -> Option<String> {
    let resp = client
        .get("https://pypi.org/pypi/demucs/json")
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?;
    let json: serde_json::Value = resp.json().await.ok()?;
    json["info"]["version"].as_str().map(String::from)
}

/// Check the latest yt-dlp version on GitHub.
pub async fn check_ytdlp_latest(client: &reqwest::Client) -> Option<String> {
    let release = github_release::latest_release(client, "yt-dlp", "yt-dlp")
        .await
        .ok()?;
    Some(release.tag_name)
}

/// Check the latest ffmpeg release tag for the update banner.
///
/// The reported "latest" must mirror what we'd actually install, so the banner
/// is honest. On macOS/Linux the download is **pinned** to
/// `ffmpeg::FFMPEG_STATIC_TAG`, so that's what we report — no false update
/// arrow for an upstream version we won't pull. On Windows we still track BtbN's
/// rolling latest (its dated autobuild tags expire, so it can't be pinned the
/// same way); the binary works regardless of version.
pub async fn check_ffmpeg_latest(client: &reqwest::Client) -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        let release = github_release::latest_release(client, "BtbN", "FFmpeg-Builds")
            .await
            .ok()?;
        Some(release.tag_name)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = client;
        Some(crate::deps::ffmpeg::FFMPEG_STATIC_TAG.to_string())
    }
}
