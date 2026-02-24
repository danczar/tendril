use std::path::Path;

use slint::{Color, Image, ModelRc, SharedPixelBuffer, SharedString, VecModel};

use tendril_core::pipeline::job::output_folder_name;
use tendril_core::progress::PipelineStage;

// Re-export the generated Slint structs
pub use crate::{QueueItemData, SearchResultData};

/// Convert core search results into a Slint model (without thumbnails).
/// Checks `output_dir` to determine if each result has already been processed.
pub fn search_results_model(
    results: &[tendril_core::youtube::search::SearchResult],
    output_dir: &Path,
) -> ModelRc<SearchResultData> {
    let items: Vec<SearchResultData> = results
        .iter()
        .map(|r| {
            let folder = output_folder_name(&r.title, Some(&r.video_id));
            let exists = output_dir.join(&folder).exists();
            SearchResultData {
                video_id: SharedString::from(&r.video_id),
                title: SharedString::from(r.title.to_lowercase()),
                channel: SharedString::from(&r.channel),
                duration: SharedString::from(format_duration(r.duration_secs)),
                thumbnail: Image::default(),
                exists,
            }
        })
        .collect();
    ModelRc::new(VecModel::from(items))
}

/// Fetch raw thumbnail bytes from a URL (Send-safe for use across threads).
pub async fn fetch_thumbnail_bytes(url: &str) -> Option<Vec<u8>> {
    let client = reqwest::Client::new();
    let bytes = client.get(url).send().await.ok()?.bytes().await.ok()?;
    Some(bytes.to_vec())
}

/// Decode raw image bytes (JPEG/PNG) into a Slint Image.
pub fn decode_image_bytes(data: &[u8]) -> Option<Image> {
    let (rgba, w, h) = decode_to_rgba(Some(data))?;
    Some(rgba_to_slint_image(&rgba, w, h))
}

/// Decode raw image bytes to RGBA pixel data (Send-safe, for use off the UI thread).
pub fn decode_to_rgba(data: Option<&[u8]>) -> Option<(Vec<u8>, u32, u32)> {
    let img = image::load_from_memory(data?).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    Some((rgba.into_raw(), w, h))
}

/// Convert raw RGBA pixel data into a Slint Image (cheap, UI-thread safe).
pub fn rgba_to_slint_image(rgba: &[u8], w: u32, h: u32) -> Image {
    let buf = SharedPixelBuffer::<slint::Rgba8Pixel>::clone_from_slice(rgba, w, h);
    Image::from_rgba8(buf)
}

/// Convert queue jobs into a Slint model.
pub fn queue_items_model(
    queue: &tendril_core::pipeline::queue::JobQueue,
    thumbnail_cache: &std::collections::HashMap<String, Vec<u8>>,
) -> ModelRc<QueueItemData> {
    let items: Vec<QueueItemData> = queue
        .iter()
        .map(|job| {
            let progress = job.progress_rx.borrow();
            let (stage_label, stage_color) = stage_display(&progress.stage);
            let thumbnail = thumbnail_cache
                .get(&job.source.thumbnail_key())
                .and_then(|bytes| decode_image_bytes(bytes))
                .unwrap_or_default();
            QueueItemData {
                job_id: job.id as i32,
                title: SharedString::from(job.source.display_name()),
                stage: SharedString::from(stage_label),
                progress: progress.fraction,
                stage_color,
                thumbnail,
            }
        })
        .collect();
    ModelRc::new(VecModel::from(items))
}

fn stage_display(stage: &PipelineStage) -> (&'static str, Color) {
    match stage {
        PipelineStage::Queued => ("Queued", Color::from_argb_u8(255, 0x9c, 0x98, 0x90)),
        PipelineStage::Downloading => ("Downloading", Color::from_argb_u8(255, 0x5b, 0x94, 0xd4)),
        PipelineStage::Splitting => ("Splitting", Color::from_argb_u8(255, 0x91, 0x79, 0xd4)),
        PipelineStage::Converting => ("Converting", Color::from_argb_u8(255, 0xd4, 0xa0, 0x56)),
        PipelineStage::Mixing => ("Mixing", Color::from_argb_u8(255, 0x5c, 0xb8, 0x96)),
        PipelineStage::Complete => ("Done", Color::from_argb_u8(255, 0x6b, 0xba, 0x7e)),
        PipelineStage::Failed => ("Failed", Color::from_argb_u8(255, 0xd4, 0x6b, 0x6b)),
    }
}

fn format_duration(secs: u32) -> String {
    if secs == 0 {
        return String::new();
    }
    let minutes = secs / 60;
    let seconds = secs % 60;
    format!("{minutes}:{seconds:02}")
}
