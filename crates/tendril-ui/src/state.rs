use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
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
    /// Monotonic counter that invalidates in-flight searches and thumbnail
    /// fetches. Bumped on every new search dispatch and on clear.
    pub search_generation: Arc<AtomicU64>,
}

impl AppState {
    pub fn new(config: Config, dirs: AppDirs) -> Self {
        Self {
            config,
            dirs,
            queue: JobQueue::new(),
            search_results: Vec::new(),
            thumbnail_cache: HashMap::new(),
            search_generation: Arc::new(AtomicU64::new(0)),
        }
    }
}

pub type SharedState = Arc<Mutex<AppState>>;

pub fn create_shared_state(config: Config, dirs: AppDirs) -> SharedState {
    Arc::new(Mutex::new(AppState::new(config, dirs)))
}
