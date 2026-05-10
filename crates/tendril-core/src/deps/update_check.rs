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

/// Check the latest ffmpeg release tag.
///
/// On Windows we download from BtbN/FFmpeg-Builds (shared build with
/// DLLs needed by torchcodec); on macOS/Linux we use the static
/// binaries from eugeneware/ffmpeg-static. The latest-version source
/// must mirror the download source so the update banner is honest.
///
/// BtbN tag format is `n7.1` / `n7.1.1`; eugeneware tags as `bX.Y.Z`
/// (currently capped at `b6.1.1` — the upstream hasn't shipped 7.x).
/// Both formats are normalized by `version_compare`.
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
        let release = github_release::latest_release(client, "eugeneware", "ffmpeg-static")
            .await
            .ok()?;
        Some(release.tag_name)
    }
}
