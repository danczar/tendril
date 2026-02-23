use tokio::sync::watch;

/// Stages of the processing pipeline for a single job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineStage {
    Queued,
    Downloading,
    Splitting,
    Converting,
    Mixing,
    Complete,
    Failed,
}

/// Progress update emitted by pipeline stages.
#[derive(Debug, Clone)]
pub struct ProgressEvent {
    pub stage: PipelineStage,
    /// 0.0 to 1.0 within the current stage.
    pub fraction: f32,
    /// Human-readable status message.
    pub message: String,
}

impl Default for ProgressEvent {
    fn default() -> Self {
        Self {
            stage: PipelineStage::Queued,
            fraction: 0.0,
            message: String::new(),
        }
    }
}

/// Create a progress watch channel pair.
///
/// The sender is held by the pipeline task; the receiver is cloned to the UI.
pub fn progress_channel() -> (watch::Sender<ProgressEvent>, watch::Receiver<ProgressEvent>) {
    watch::channel(ProgressEvent::default())
}
