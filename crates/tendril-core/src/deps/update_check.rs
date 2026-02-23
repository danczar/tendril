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

/// Check the latest ffmpeg-static version on GitHub.
pub async fn check_ffmpeg_latest(client: &reqwest::Client) -> Option<String> {
    let release =
        github_release::latest_release(client, "eugeneware", "ffmpeg-static")
            .await
            .ok()?;
    Some(release.tag_name)
}
