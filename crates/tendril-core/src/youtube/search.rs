use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use rustypipe::client::RustyPipe;
use rustypipe::model::TrackItem;

/// A single YouTube Music search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub video_id: String,
    pub title: String,
    pub channel: String,
    pub duration_secs: u32,
    pub thumbnail_url: String,
}

/// Search YouTube Music for songs and music videos matching `query`.
///
/// Queries tracks and videos separately (each tab includes duration data
/// that the combined endpoint omits) and interleaves the results,
/// deduplicating by video ID.
pub async fn search(query: &str) -> Result<Vec<SearchResult>, crate::error::YoutubeError> {
    let rp = RustyPipe::new();
    let q = rp.query();

    let (tracks, videos) = tokio::join!(
        q.music_search_tracks(query),
        q.music_search_videos(query),
    );

    let mut seen = HashSet::new();

    let track_items = match tracks {
        Ok(r) => r.items.items,
        Err(e) => {
            if videos.is_err() {
                return Err(crate::error::YoutubeError::Search(format!(
                    "search failed for both tabs: tracks={e}, videos={:?}",
                    videos.as_ref().err().map(|e| e.to_string())
                )));
            }
            tracing::warn!("music_search_tracks failed (continuing with video results only): {e}");
            Vec::new()
        }
    };
    let video_items = match videos {
        Ok(r) => r.items.items,
        Err(e) => {
            // tracks was Ok if we got here (the both-fail case is handled above).
            tracing::warn!("music_search_videos failed (continuing with track results only): {e}");
            Vec::new()
        }
    };

    // Interleave: video, song, video, song, ...
    let mut items = Vec::new();
    let mut ti = track_items.into_iter();
    let mut vi = video_items.into_iter();
    loop {
        let v = vi.next();
        let t = ti.next();
        if v.is_none() && t.is_none() {
            break;
        }
        for item in [v, t].into_iter().flatten() {
            if seen.insert(item.id.clone()) {
                items.push(map_track_item(item));
            }
        }
    }

    Ok(items)
}

fn map_track_item(t: TrackItem) -> SearchResult {
    let channel = t
        .artists
        .iter()
        .map(|a| a.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    SearchResult {
        thumbnail_url: t
            .cover
            .first()
            .map(|c| c.url.clone())
            .unwrap_or_else(|| {
                format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", t.id)
            }),
        video_id: t.id,
        title: t.name,
        channel,
        duration_secs: t.duration.unwrap_or(0),
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
