use serde::{Deserialize, Serialize};

use rustypipe::client::RustyPipe;
use rustypipe::model::VideoItem;

/// A single YouTube search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub video_id: String,
    pub title: String,
    pub channel: String,
    pub duration_secs: u32,
    pub thumbnail_url: String,
}

/// Search YouTube for videos matching `query`.
pub async fn search(query: &str) -> Result<Vec<SearchResult>, crate::error::YoutubeError> {
    let rp = RustyPipe::new();
    let result: rustypipe::model::SearchResult<VideoItem> = rp
        .query()
        .search(query)
        .await
        .map_err(|e| crate::error::YoutubeError::Search(e.to_string()))?;

    let items = result
        .items
        .items
        .into_iter()
        .map(|v| map_video_item(v))
        .collect();

    Ok(items)
}

fn map_video_item(v: VideoItem) -> SearchResult {
    SearchResult {
        thumbnail_url: v
            .thumbnail
            .first()
            .map(|t| t.url.clone())
            .unwrap_or_else(|| {
                format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", v.id)
            }),
        video_id: v.id,
        title: v.name,
        channel: v.channel.map(|c| c.name).unwrap_or_default(),
        duration_secs: v.duration.unwrap_or(0),
    }
}

/// Get autocomplete suggestions for a partial query.
pub async fn autocomplete(partial: &str) -> Result<Vec<String>, crate::error::YoutubeError> {
    let rp = RustyPipe::new();
    rp.query()
        .search_suggestion(partial)
        .await
        .map_err(|e| crate::error::YoutubeError::Search(e.to_string()))
}
