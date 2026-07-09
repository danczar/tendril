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

/// Check the latest ffmpeg version for the update banner.
///
/// The reported "latest" must mirror what we'd actually install, so the banner
/// is honest. On macOS/Linux the download is **pinned** to
/// `ffmpeg::FFMPEG_STATIC_TAG`, so that's what we report — normalized for
/// display (tag `b6.1.1` → `6.1.1`, the form the installed binary prints).
/// On Windows there is nothing comparable to report: BtbN's rolling autobuild
/// is tagged literally `"latest"`, which can never equal an installed version
/// and would keep the update arrow permanently on.
pub async fn check_ffmpeg_latest(client: &reqwest::Client) -> Option<String> {
    let _ = client;
    #[cfg(target_os = "windows")]
    {
        None
    }
    #[cfg(not(target_os = "windows"))]
    {
        Some(
            crate::deps::ffmpeg::FFMPEG_STATIC_TAG
                .trim_start_matches('b')
                .to_string(),
        )
    }
}
