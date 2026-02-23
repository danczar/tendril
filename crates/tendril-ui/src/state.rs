use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tendril_core::config::Config;
use tendril_core::dirs::AppDirs;
use tendril_core::pipeline::queue::JobQueue;
use tendril_core::youtube::search::SearchResult;

/// Shared application state accessible from both UI callbacks and async tasks.
pub struct AppState {
    pub config: Config,
    pub dirs: AppDirs,
    pub queue: JobQueue,
    /// All search results from the last query.
    pub search_results: Vec<SearchResult>,
    /// Raw thumbnail bytes keyed by video_id (Send-safe, decoded on UI thread).
    pub thumbnail_cache: HashMap<String, Vec<u8>>,
}

impl AppState {
    pub fn new(config: Config, dirs: AppDirs) -> Self {
        Self {
            config,
            dirs,
            queue: JobQueue::new(),
            search_results: Vec::new(),
            thumbnail_cache: HashMap::new(),
        }
    }
}

pub type SharedState = Arc<Mutex<AppState>>;

pub fn create_shared_state(config: Config, dirs: AppDirs) -> SharedState {
    Arc::new(Mutex::new(AppState::new(config, dirs)))
}
