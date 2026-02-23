use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::watch;

use crate::progress::ProgressEvent;

static NEXT_JOB_ID: AtomicU64 = AtomicU64::new(1);

/// Where the audio originates from.
#[derive(Debug, Clone)]
pub enum JobSource {
    Youtube {
        video_id: String,
        title: String,
    },
    LocalFile {
        path: PathBuf,
    },
}

impl JobSource {
    pub fn display_name(&self) -> &str {
        match self {
            JobSource::Youtube { title, .. } => title,
            JobSource::LocalFile { path } => {
                path.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("unknown")
            }
        }
    }

    pub fn video_id(&self) -> Option<&str> {
        match self {
            JobSource::Youtube { video_id, .. } => Some(video_id),
            JobSource::LocalFile { .. } => None,
        }
    }
}

/// Compute the output folder name for a job.
///
/// YouTube sources get `"{title} ({video_id})"`, local files use the file stem.
/// The result is lowercased and sanitized for filesystem safety.
pub fn output_folder_name(title: &str, video_id: Option<&str>) -> String {
    let base = match video_id {
        Some(id) => format!("{} ({})", title, id),
        None => title.to_string(),
    };
    base.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_")
}

/// A single processing job in the queue.
pub struct Job {
    pub id: u64,
    pub source: JobSource,
    pub progress_tx: Arc<watch::Sender<ProgressEvent>>,
    pub progress_rx: watch::Receiver<ProgressEvent>,
    pub cancel_tx: watch::Sender<bool>,
    pub cancel_rx: watch::Receiver<bool>,
}

impl Job {
    pub fn new(source: JobSource) -> Self {
        let (progress_tx, progress_rx) = crate::progress::progress_channel();
        let (cancel_tx, cancel_rx) = watch::channel(false);
        Self {
            id: NEXT_JOB_ID.fetch_add(1, Ordering::Relaxed),
            source,
            progress_tx: Arc::new(progress_tx),
            progress_rx,
            cancel_tx,
            cancel_rx,
        }
    }
}
