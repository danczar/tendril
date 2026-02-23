use std::path::PathBuf;

/// Top-level error type for tendril-core.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error(transparent)]
    Dependency(#[from] DependencyError),

    #[error(transparent)]
    Youtube(#[from] YoutubeError),

    #[error(transparent)]
    Splitter(#[from] SplitterError),

    #[error(transparent)]
    Audio(#[from] AudioError),

    #[error(transparent)]
    Pipeline(#[from] PipelineError),
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config from {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("failed to write config to {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },

    #[error("failed to parse config: {0}")]
    Parse(#[from] toml::de::Error),

    #[error("failed to serialize config: {0}")]
    Serialize(#[from] toml::ser::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum DependencyError {
    #[error("HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("GitHub API error: {message}")]
    GitHubApi { message: String },

    #[error("no compatible release found for {tool}")]
    NoRelease { tool: String },

    #[error("failed to extract archive: {0}")]
    Extract(std::io::Error),

    #[error("binary not found after install: {path}")]
    BinaryNotFound { path: PathBuf },
}

#[derive(Debug, thiserror::Error)]
pub enum YoutubeError {
    #[error("search failed: {0}")]
    Search(String),

    #[error("download failed for {url}: {message}")]
    Download { url: String, message: String },

    #[error("yt-dlp process exited with code {code}: {stderr}")]
    Process { code: i32, stderr: String },
}

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("ffmpeg conversion failed: {message}")]
    Conversion { message: String },

    #[error("ffmpeg process exited with code {code}: {stderr}")]
    FfmpegProcess { code: i32, stderr: String },

    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),
}

#[derive(Debug, thiserror::Error)]
pub enum SplitterError {
    #[error("inference failed: {0}")]
    Inference(String),

    #[error("model not found: {path}")]
    ModelNotFound { path: PathBuf },

    #[error("unsupported model variant: {0}")]
    UnsupportedModel(String),
}

#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error("job {job_id} not found")]
    JobNotFound { job_id: u64 },

    #[error("pipeline stage failed: {stage} — {message}")]
    StageFailed { stage: String, message: String },

    #[error("job cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, Error>;
